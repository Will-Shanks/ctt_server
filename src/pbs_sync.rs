use crate::cluster::ClusterTrait;
#[cfg(feature = "gust")]
use crate::cluster::Gust as Cluster;
use crate::conf::Conf;
use crate::entities;
use crate::entities::issue::IssueStatus;
use crate::entities::issue::ToOffline;
use crate::entities::prelude::Target;
use crate::entities::target::TargetStatus;
use crate::model::mutation;
use sea_orm::prelude::Expr;
use sea_orm::{ActiveModelTrait, ActiveValue, ColumnTrait, QueryFilter, QuerySelect};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

use sea_orm::DatabaseConnection;
use std::time::Duration;
use tokio::time;
use tracing::{debug, info, instrument, trace, warn};

#[instrument(skip(db, conf))]
pub async fn pbs_sync(db: Arc<DatabaseConnection>, conf: Conf) {
    let mut interval = time::interval(Duration::from_secs(conf.poll_interval));
    // don't let ticks stack up if a sync takes longer than interval
    interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
    loop {
        interval.tick().await;
        let db = db.as_ref();
        info!("performing sync with pbs");
        let (tx, rx) = mpsc::channel(5);
        tokio::spawn(crate::slack_updater(rx, conf.clone()));
        let pbs_srv = pbs::Server::new();
        let pbs_node_state = Cluster::nodes_status(&pbs_srv, &tx).await;
        let mut ctt_node_state = get_ctt_nodes(db).await;

        //handle any pbs nodes not in ctt
        pbs_node_state
            .keys()
            .filter(|t| !ctt_node_state.contains_key(*t))
            .collect::<Vec<&String>>()
            .iter()
            .for_each(|t| {
                ctt_node_state.insert(t.to_string(), TargetStatus::Online);
            });

        // sync ctt and pbs
        for (target, old_state) in &ctt_node_state {
            if let Some((new_state, pbs_comment)) = pbs_node_state.get(target) {
                handle_transition(target, pbs_comment, old_state, new_state, &pbs_srv, db, &tx)
                    .await;
            } else {
                warn!("{} not found in pbs", target);
                let new_issue = crate::model::NewIssue::new(
                    None,
                    "Node not found in pbs".to_string(),
                    "Node not found in pbs".to_string(),
                    target.to_string(),
                    None,
                );
                //TODO need to add a way to delete a node from ctt
                mutation::issue_open(&new_issue, "ctt", db, &tx)
                    .await
                    .unwrap();
            }
        }
        debug!("pbs sync complete");
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
pub async fn desired_state(target: &str, db: &DatabaseConnection) -> (TargetStatus, String) {
    let t = entities::target::Entity::from_name(target, db).await;
    let t = if let Some(tmp) = t {
        tmp
    } else {
        //TODO check if t is a valid node
        //if not give a warning and return (TargetStatus::Offline, "Invalid node")
        info!("creating target {}", &target);
        Target::create_target(target, TargetStatus::Online, db)
            .await
            .unwrap()
    };
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
    for c in Cluster::siblings(target) {
        let t = entities::target::Entity::from_name(&c, db).await;
        let t = if let Some(tmp) = t {
            tmp
        } else {
            //TODO check if t is a valid node
            //if not give a warning and return (TargetStatus::Offline, "Invalid node")
            info!("creating target {}", &c);
            Target::create_target(&c, TargetStatus::Online, db)
                .await
                .unwrap()
        };
        if t.issues()
            .filter(entities::issue::Column::Status.eq(IssueStatus::Open))
            .filter(entities::issue::Column::ToOffline.eq(Some(ToOffline::Card)))
            .one(db)
            .await
            .unwrap()
            .is_some()
        {
            debug!("Offline due to card  wide ticket");
            return (TargetStatus::Offline, format!("{} sibling", &target));
        }
    }
    for c in Cluster::cousins(target) {
        let t = entities::target::Entity::from_name(&c, db).await;
        let t = if let Some(tmp) = t {
            tmp
        } else {
            //TODO check if t is a valid node
            //if not give a warning and return (TargetStatus::Offline, "Invalid node")
            info!("creating target {}", &c);
            Target::create_target(&c, TargetStatus::Online, db)
                .await
                .unwrap()
        };
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
pub async fn close_open_issues(target: &str, db: &DatabaseConnection) {
    for issue in entities::target::Entity::from_name(target, db)
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

#[instrument(skip(pbs_srv, db, tx))]
async fn handle_transition(
    target: &str,
    new_comment: &str,
    old_state: &TargetStatus,
    new_state: &TargetStatus,
    pbs_srv: &pbs::Server,
    db: &DatabaseConnection,
    tx: &mpsc::Sender<String>,
) {
    let (expected_state, comment) = desired_state(target, db).await;

    //dont use old_state to figure out how to handle nodes
    //things could have changed between when it was collected and now, so only consider
    //the current state (new_state) and the expected_state
    let final_state = match expected_state {
        TargetStatus::Draining => panic!("Expected state is never Draining"),
        TargetStatus::Online => {
            if *new_state == TargetStatus::Online {
                TargetStatus::Online
            } else {
                let new_issue = crate::model::NewIssue::new(
                    None,
                    new_comment.to_string(),
                    new_comment.to_string(),
                    target.to_string(),
                    None,
                );
                info!("opening issue for {}: {}", target, new_comment);
                mutation::issue_open(&new_issue, "ctt", db, tx)
                    .await
                    .unwrap();
                *new_state
            }
        }
        TargetStatus::Offline => match new_state {
            TargetStatus::Draining => TargetStatus::Draining,
            TargetStatus::Offline => TargetStatus::Offline,
            state => {
                info!("{} found in state {:?}, expected offline", target, state);
                Cluster::offline_node(target, &comment, "ctt", pbs_srv, tx)
                    .await
                    .unwrap();
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
                let _ = tx
                    .send(format!(
                        "ctt: Closing issues for {}, node found online",
                        target
                    ))
                    .await;
                close_open_issues(target, db).await;
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
        let node = entities::target::Entity::from_name(target, db)
            .await
            .unwrap();
        let mut updated_target: entities::target::ActiveModel = node.into();
        updated_target.status = ActiveValue::Set(final_state);
        updated_target.update(db).await.unwrap();
    }
}
