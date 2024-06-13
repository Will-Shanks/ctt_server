use crate::changelog;
use crate::cluster::scheduler::PbsScheduler;
use crate::cluster::ClusterTrait;
use crate::cluster::RegexCluster;
use crate::conf::Conf;
use crate::entities;
use crate::entities::issue::IssueStatus;
use crate::entities::issue::ToOffline;
use crate::entities::target::TargetStatus;
use crate::model::mutation;
use crate::ChangeLogMsg;
use pbs::Server;
use sea_orm::prelude::Expr;
use sea_orm::{ActiveModelTrait, ActiveValue, ColumnTrait, QueryFilter, QuerySelect};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use sea_orm::DatabaseConnection;
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time;
use tracing::{debug, info, instrument, trace, warn};

pub static PBS_LOCK: LazyLock<Mutex<u32>> = LazyLock::new(|| Mutex::new(0));

#[instrument(skip(db, conf))]
pub async fn pbs_sync(db: Arc<DatabaseConnection>, conf: Conf) {
    let mut interval = time::interval(Duration::from_secs(conf.poll_interval));
    let cluster = RegexCluster::new(conf.node_types.clone(), PbsScheduler::new(Server::new()));
    // don't let ticks stack up if a sync takes longer than interval
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        // don't want multiple ctt threads messing with scheduler concurrently
        let sched_lock = PBS_LOCK.lock().await;
        let db = db.as_ref();
        info!("performing sync with pbs");
        let (tx, rx) = mpsc::channel(5);
        tokio::spawn(changelog::slack_updater(rx, conf.clone()));
        let pbs_node_state = cluster.nodes_status();
        if let Err(e) = pbs_node_state {
            warn!("could not get node state from cluster: {}", e);
            continue;
        }
        let pbs_node_state = pbs_node_state.unwrap();
        let mut ctt_node_state = get_ctt_nodes(db).await;

        //add any pbs nodes not in ctt into ctt for tracking
        pbs_node_state
            .keys()
            .filter(|t| !ctt_node_state.contains_key(*t))
            .filter(|t| cluster.real_node(t))
            .collect::<Vec<&String>>()
            .iter()
            .for_each(|t| {
                ctt_node_state.insert(t.to_string(), TargetStatus::Online);
            });

        // sync ctt and pbs
        for (target, old_state) in &ctt_node_state {
            if let Some((new_state, pbs_comment)) = pbs_node_state.get(target) {
                handle_transition(target, pbs_comment, old_state, new_state, db, &tx, &cluster)
                    .await;
            } else {
                warn!("{} not found in pbs", target);
                if let Some(new_issue) = crate::model::NewIssue::new(
                    None,
                    "Node not found in pbs".to_string(),
                    "Node not found in pbs".to_string(),
                    target.to_string(),
                    None,
                    &cluster,
                ) {
                    mutation::issue_open(&new_issue, "ctt", db, &tx, &cluster)
                        .await
                        .unwrap();
                }
            }
        }
        info!("pbs sync complete");
        drop(sched_lock);
    }
}

#[instrument(skip(db))]
pub async fn get_ctt_nodes(db: &DatabaseConnection) -> HashMap<String, TargetStatus> {
    let ctt_node_state = entities::target::Entity::all()
        .select_only()
        .columns([
            entities::target::Column::Name,
            entities::target::Column::Status,
            entities::target::Column::Id,
        ])
        .all(db)
        .await
        .unwrap();
    ctt_node_state
        .iter()
        .map(|n| (n.name.clone(), n.status))
        .collect()
}

#[instrument(skip(db))]
pub async fn desired_state(
    target: &str,
    db: &DatabaseConnection,
    cluster: &RegexCluster,
) -> (TargetStatus, String) {
    let t = entities::target::Entity::from_name(target, db, cluster).await;
    let t = match t {
        None => return (TargetStatus::Offline, "Not a real node".to_string()),
        Some(t) => {
            if let Some(iss) = t
                .issues()
                .filter(entities::issue::Column::Status.eq(IssueStatus::Open))
                .filter(Expr::col(entities::issue::Column::ToOffline).is_not_null())
                .one(db)
                .await
                .unwrap()
            {
                debug!("Offline due to node ticket");
                return (TargetStatus::Offline, iss.title);
            }
            t
        }
    };
    for c in cluster.siblings(target) {
        match entities::target::Entity::from_name(&c, db, cluster).await {
            None => warn!("expected sibling {} doesn't exist", c),
            Some(t) => {
                if t.issues()
                    .filter(entities::issue::Column::Status.eq(IssueStatus::Open))
                    .filter(entities::issue::Column::ToOffline.eq(Some(ToOffline::Card)))
                    .one(db)
                    .await
                    .unwrap()
                    .is_some()
                {
                    debug!("Offline due to card wide ticket");
                    return (TargetStatus::Offline, format!("{} sibling", &target));
                }
            }
        };
    }
    for c in cluster.cousins(target) {
        match entities::target::Entity::from_name(&c, db, cluster).await {
            None => warn!("expected sibling {} doesn't exist", c),
            Some(t) => {
                if t.issues()
                    .filter(entities::issue::Column::Status.eq(IssueStatus::Open))
                    .filter(entities::issue::Column::ToOffline.eq(Some(ToOffline::Blade)))
                    .one(db)
                    .await
                    .unwrap()
                    .is_some()
                {
                    debug!("Offline due to blade wide ticket");
                    return (TargetStatus::Offline, format!("{} sibling", &target));
                }
            }
        };
    }
    if let Some(iss) = t
        .issues()
        .filter(entities::issue::Column::Status.eq(IssueStatus::Open))
        .filter(Expr::col(entities::issue::Column::ToOffline).is_null())
        .one(db)
        .await
        .unwrap()
    {
        debug!("Down due to node ticket");
        return (TargetStatus::Down, iss.title);
    }
    trace!("Online due to no related tickets");
    (TargetStatus::Online, "".to_string())
}

#[instrument(skip(db))]
pub async fn close_open_issues(target: &str, db: &DatabaseConnection, cluster: &RegexCluster) {
    for issue in entities::target::Entity::from_name(target, db, cluster)
        .await
        .unwrap()
        .issues()
        .filter(entities::issue::Column::Status.eq(IssueStatus::Open))
        .all(db)
        .await
        .unwrap()
    {
        let id = issue.id;
        let mut i: entities::issue::ActiveModel = issue.into();
        i.status = ActiveValue::Set(IssueStatus::Closed);
        i.update(db).await.unwrap();
        let c = entities::comment::ActiveModel {
            created_by: ActiveValue::Set("ctt".to_string()),
            comment: ActiveValue::Set("node found up, assuming issue is resolved".to_string()),
            issue_id: ActiveValue::Set(id),
            ..Default::default()
        };
        c.insert(db).await.unwrap();
    }
}

#[instrument(skip(db, tx))]
#[allow(clippy::too_many_arguments)]
async fn handle_transition(
    target: &str,
    new_comment: &str,
    old_state: &TargetStatus,
    new_state: &TargetStatus,
    db: &DatabaseConnection,
    tx: &mpsc::Sender<ChangeLogMsg>,
    cluster: &RegexCluster,
) {
    let (expected_state, comment) = desired_state(target, db, cluster).await;

    //dont use old_state to figure out how to handle nodes
    //things could have changed between when it was collected and now, so only consider
    //the current state (new_state) and the expected_state
    let final_state = match expected_state {
        TargetStatus::Draining => panic!("Expected state is never Draining"),
        TargetStatus::Online => {
            if *new_state == TargetStatus::Online {
                TargetStatus::Online
            } else {
                // expected node to be online, but it wasn't so open an issue
                // we know no issues are currently open since expected state
                // would not be online if there were
                if let Some(new_issue) = crate::model::NewIssue::new(
                    None,
                    new_comment.to_string(),
                    new_comment.to_string(),
                    target.to_string(),
                    None,
                    cluster,
                ) {
                    info!("opening issue for {}: {}", target, new_comment);
                    mutation::issue_open(&new_issue, "ctt", db, tx, cluster)
                        .await
                        .unwrap();
                }
                *new_state
            }
        }
        TargetStatus::Offline => match new_state {
            TargetStatus::Draining => TargetStatus::Draining,
            TargetStatus::Offline => TargetStatus::Offline,
            state => {
                info!("{} found in state {:?}, expected offline", target, state);
                cluster.offline_node(target, &comment).unwrap();
                let _ = tx
                    .send(ChangeLogMsg::Offline {
                        target: target.to_string(),
                    })
                    .await;
                if *state == TargetStatus::Down {
                    TargetStatus::Offline
                } else {
                    // node was online, might have running jobs
                    TargetStatus::Draining
                }
            }
        },
        TargetStatus::Down => match new_state {
            TargetStatus::Draining => TargetStatus::Draining,
            TargetStatus::Down => TargetStatus::Down,
            TargetStatus::Offline => TargetStatus::Offline,
            TargetStatus::Online => {
                info!("closing open issues for {}", target);
                // know it is safe to simply close all issue open against the node because
                // expected status would be Offline if there were any issues with ToOffline set
                close_open_issues(target, db, cluster).await;
                TargetStatus::Online
            }
        },
    };
    //dont update state if it hasn't changed
    if *old_state != final_state {
        debug!(
            "{}: current: {:?}, expected: {:?}, final: {:?}",
            target, new_state, expected_state, final_state
        );
        let node = if let Some(tmp) = entities::target::Entity::from_name(target, db, cluster).await
        {
            tmp
        } else {
            warn!("trying to update state for fake node {}", target);
            return;
        };

        let mut updated_target: entities::target::ActiveModel = node.into();
        updated_target.status = ActiveValue::Set(final_state);
        updated_target.update(db).await.unwrap();
    }
}