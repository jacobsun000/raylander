use std::process::Command;

use anyhow::{Context, Result, bail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMethodStatus {
    Inactive,
    Active,
    Unavailable,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InputMethodState {
    pub status: InputMethodStatus,
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InputMethodAction {
    Toggle,
}

pub struct Fcitx5Backend;

impl Fcitx5Backend {
    pub fn current(&self) -> Result<InputMethodState> {
        let status = run_fcitx(&[])?;
        let name = run_fcitx(&["-n"])?;
        parse_input_method(&status, &name)
    }

    pub fn apply(&self, action: &InputMethodAction) -> Result<()> {
        match action {
            InputMethodAction::Toggle => run_fcitx(&["-t"]),
        }?;
        Ok(())
    }
}

fn run_fcitx(args: &[&str]) -> Result<String> {
    let output = Command::new("fcitx5-remote")
        .args(args)
        .output()
        .with_context(|| format!("failed to execute fcitx5-remote {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "fcitx5-remote {} exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    String::from_utf8(output.stdout).context("fcitx5-remote returned non-UTF-8 output")
}

pub fn parse_input_method(status: &str, name: &str) -> Result<InputMethodState> {
    let status = match status.trim() {
        "0" => InputMethodStatus::Unavailable,
        "1" => InputMethodStatus::Inactive,
        "2" => InputMethodStatus::Active,
        value => bail!("unexpected fcitx5-remote status: {value}"),
    };
    let name = name.trim();
    Ok(InputMethodState {
        status,
        name: (!name.is_empty()).then(|| name.to_owned()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_states_and_optional_name() {
        assert_eq!(
            parse_input_method("2\n", "keyboard-us\n").unwrap(),
            InputMethodState {
                status: InputMethodStatus::Active,
                name: Some("keyboard-us".into())
            }
        );
        assert_eq!(
            parse_input_method("1", "").unwrap(),
            InputMethodState {
                status: InputMethodStatus::Inactive,
                name: None
            }
        );
        assert_eq!(
            parse_input_method("0", "pinyin").unwrap().status,
            InputMethodStatus::Unavailable
        );
    }

    #[test]
    fn rejects_empty_or_unknown_status() {
        assert!(parse_input_method("", "keyboard-us").is_err());
        assert!(parse_input_method("3", "keyboard-us").is_err());
        assert!(parse_input_method("active", "keyboard-us").is_err());
    }
}
