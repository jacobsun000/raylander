use std::{collections::HashMap, process::Command};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStatus {
    pub workspace: String,
    pub pane: String,
    pub pane_id: String,
    pub status: String,
}

#[derive(Deserialize)]
struct Response<T> {
    result: T,
}

#[derive(Deserialize)]
struct AgentList {
    agents: Vec<Agent>,
}

#[derive(Deserialize)]
struct Agent {
    agent_status: String,
    pane_id: String,
    workspace_id: String,
}

#[derive(Deserialize)]
struct WorkspaceList {
    workspaces: Vec<Workspace>,
}

#[derive(Deserialize)]
struct Workspace {
    label: String,
    workspace_id: String,
}

pub struct HerdrBackend;

impl HerdrBackend {
    pub fn agents(&self) -> Result<Vec<AgentStatus>> {
        let workspaces: Response<WorkspaceList> = query(&["workspace", "list"])?;
        let labels: HashMap<_, _> = workspaces
            .result
            .workspaces
            .into_iter()
            .map(|workspace| (workspace.workspace_id, workspace.label))
            .collect();
        let agents: Response<AgentList> = query(&["agent", "list"])?;
        let mut agents = agents
            .result
            .agents
            .into_iter()
            .map(|agent| AgentStatus {
                workspace: labels
                    .get(&agent.workspace_id)
                    .cloned()
                    .unwrap_or(agent.workspace_id),
                pane: agent
                    .pane_id
                    .rsplit_once(":p")
                    .map_or(agent.pane_id.clone(), |(_, pane)| pane.to_owned()),
                pane_id: agent.pane_id,
                status: agent.agent_status,
            })
            .collect::<Vec<_>>();
        agents.sort_by(|a, b| (&a.workspace, &a.pane).cmp(&(&b.workspace, &b.pane)));
        Ok(agents)
    }

    pub fn focus(&self, pane_id: &str) -> Result<()> {
        let output = Command::new("herdr")
            .args(["agent", "focus", pane_id])
            .output()
            .context("failed to execute herdr")?;
        if !output.status.success() {
            bail!("herdr agent focus failed");
        }
        Ok(())
    }
}

fn query<T: for<'de> Deserialize<'de>>(args: &[&str]) -> Result<T> {
    let output = Command::new("herdr")
        .args(args)
        .output()
        .context("failed to execute herdr")?;
    if !output.status.success() {
        bail!("herdr {} failed", args.join(" "));
    }
    serde_json::from_slice(&output.stdout).context("herdr returned invalid JSON")
}
