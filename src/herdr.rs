use std::{collections::HashMap, process::Command};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentStatus {
    pub workspace: String,
    pub pane: String,
    pub pane_id: String,
    pub workspace_number: u32,
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
    number: u32,
    workspace_id: String,
}

pub struct HerdrBackend;

impl HerdrBackend {
    pub fn agents(&self) -> Result<Vec<AgentStatus>> {
        let workspaces: Response<WorkspaceList> = query(&["workspace", "list"])?;
        let workspaces: HashMap<_, _> = workspaces
            .result
            .workspaces
            .into_iter()
            .map(|workspace| (workspace.workspace_id, (workspace.label, workspace.number)))
            .collect();
        let agents: Response<AgentList> = query(&["agent", "list"])?;
        let mut agents = agents
            .result
            .agents
            .into_iter()
            .map(|agent| AgentStatus {
                workspace: workspaces
                    .get(&agent.workspace_id)
                    .map(|(label, _)| label.clone())
                    .unwrap_or_else(|| agent.workspace_id.clone()),
                pane: agent
                    .pane_id
                    .rsplit_once(":p")
                    .map_or(agent.pane_id.clone(), |(_, pane)| pane.to_owned()),
                pane_id: agent.pane_id,
                workspace_number: workspaces
                    .get(&agent.workspace_id)
                    .map_or(1, |(_, number)| *number),
                status: agent.agent_status,
            })
            .collect::<Vec<_>>();
        agents.sort_by(|a, b| (&a.workspace, &a.pane).cmp(&(&b.workspace, &b.pane)));
        Ok(agents)
    }

    pub fn focus(&self, pane_id: &str, workspace_number: u32) -> Result<()> {
        let output = Command::new("herdr")
            .args(["agent", "focus", pane_id])
            .output()
            .context("failed to execute herdr")?;
        if !output.status.success() {
            bail!("herdr agent focus failed");
        }
        let status = Command::new("hyprctl")
            .args([
                "dispatch",
                &format!("hl.dsp.focus({{workspace={workspace_number}}})"),
            ])
            .status()
            .context("failed to execute hyprctl")?;
        if !status.success() {
            bail!("hyprctl workspace focus failed");
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
