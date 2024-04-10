use crate::entities::target::TargetStatus;
#[cfg(feature = "pbs")]
use pbs::{Attrl, Op, Server};
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::instrument;
use tracing::{info, warn};

#[instrument(skip(pbs_srv))]
pub async fn nodes_status(
    pbs_srv: &Server,
    tx: &mpsc::Sender<String>,
) -> Result<HashMap<String, (TargetStatus, String)>, ()> {
    //TODO filter stat attribs (just need hostname, jobs, and state)
    //TODO need to handle err
    //TODO consider calling pbs_srv.stat_vnode from a spawn_blocking task
    //TODO add a timeout
    let mut resp = HashMap::new();
    #[cfg(feature = "pbs")]
    let vnode_stat = pbs_srv.stat_vnode(&None, None);
    if vnode_stat.is_err() {
        return Err(());
    }
    for n in vnode_stat.unwrap().resources.iter() {
        let name = n.name();
        let jobs = {
            if let Some(Attrl::Value(Op::Default(j))) = n.attribs().get("jobs") {
                !j.is_empty()
            } else {
                false
            }
        };
        #[allow(clippy::manual_unwrap_or_default)]
        let comment =
            if let Some(pbs::Attrl::Value(pbs::Op::Default(c))) = n.attribs().get("comment") {
                c
            } else {
                ""
            };
        let state = match n.attribs().get("state").unwrap() {
            Attrl::Value(Op::Default(j)) => j,
            x => {
                println!("{:?}", x);
                panic!("bad state");
            }
        };
        let state = match state.as_str() {
            //order matters, before "down" to capture down,offline nodes
            x if x.contains("offline") => {
                if jobs {
                    TargetStatus::Draining
                } else {
                    TargetStatus::Offline
                }
            }
            x if x.contains("down") => {
                if jobs {
                    TargetStatus::Draining
                } else {
                    TargetStatus::Down
                }
            }
            //job-excl or resv-excl
            x if x.contains("exclusive") => TargetStatus::Online,
            "job-busy" => TargetStatus::Online,
            "free" => TargetStatus::Online,
            x => {
                warn!("unrecognized node state, '{}'", x);
                //TODO FIXME handle err
                //TODO should we really offline nodes randomly while checking node status?
                if let Err(e) = pbs_srv.offline_vnode(&name, None) {
                    warn!("Error offlining node {}: {}", name, e);
                }
                let _ = tx
                    .send(format!("ctt offlining: {}, {}", name, comment))
                    .await;
                if jobs {
                    TargetStatus::Draining
                } else {
                    TargetStatus::Down
                }
            }
        };
        resp.insert(name, (state, comment.to_string()));
    }
    Ok(resp)
}

pub async fn release_node(
    target: &str,
    operator: &str,
    pbs_srv: &Server,
    tx: &mpsc::Sender<String>,
) -> Result<(), ()> {
    info!("{} resuming node {}", operator, target);
    #[cfg(feature = "pbs")]
    if pbs_srv.clear_vnode(target, Some("")).is_err() {
        return Err(());
    }
    let _ = tx
        .send(format!("{} onlining node: {}", operator, target))
        .await;
    Ok(())
}

pub async fn offline_node(
    target: &str,
    comment: &str,
    operator: &str,
    pbs_srv: &Server,
    tx: &mpsc::Sender<String>,
) -> Result<(), ()> {
    info!("{} offlining: {}, {}", operator, target, comment);
    #[cfg(feature = "pbs")]
    if pbs_srv.offline_vnode(target, Some(comment)).is_err() {
        return Err(());
    }
    let _ = tx
        .send(format!("{} offlining: {}, {}", operator, target, comment))
        .await;
    Ok(())
}
