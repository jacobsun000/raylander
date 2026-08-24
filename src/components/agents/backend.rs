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

#[derive(Deserialize)]
struct HyprClient {
    address: String,
    class: String,
    title: String,
}

pub struct HerdrBackend;

impl HerdrBackend {
    pub fn agents(&self) -> Result<Vec<AgentStatus>> {
        let workspaces: Response<WorkspaceList> = query(&["workspace", "list"])?;
        let workspaces: HashMap<_, _> = workspaces
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
                workspace: workspaces
                    .get(&agent.workspace_id)
                    .cloned()
                    .unwrap_or_else(|| agent.workspace_id.clone()),
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
        let clients = Command::new("hyprctl")
            .args(["-j", "clients"])
            .output()
            .context("failed to query Hyprland clients")?;
        let clients: Vec<HyprClient> = serde_json::from_slice(&clients.stdout)
            .context("hyprctl returned invalid client JSON")?;
        let client = clients
            .iter()
            .find(|client| {
                client.title.to_ascii_lowercase().starts_with("herdr")
                    || client.class.eq_ignore_ascii_case("herdr")
            })
            .context("Herdr Hyprland window was not found")?;
        let status = Command::new("hyprctl")
            .args([
                "dispatch",
                &format!("hl.dsp.focus({{window=\"address:{}\"}})", client.address),
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
