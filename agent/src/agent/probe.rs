//! Environment probe — detects available runtimes at startup.
//!
//! Probes binaries like python3, pip, uv, node via which/where,
//! caches results, and generates a `## Runtime Environment` prompt section.

use std::collections::HashMap;

use crate::config::AgentSection;

/// Information about a single binary.
#[derive(Debug, Clone)]
pub struct BinaryInfo {
    pub available: bool,
    pub version: Option<String>,
    #[allow(dead_code)]
    pub path: Option<String>,
}

/// Holds probe results for all binaries.
#[derive(Debug, Clone)]
pub struct EnvironmentProbe {
    pub binaries: HashMap<String, BinaryInfo>,
}

/// Default binaries to probe.
///
/// Platform differences:
/// - Unix: `which` is used, `python3` is the canonical name.
/// - Windows: `where` is used. Python installer only creates `python.exe`
///   (no python3). We probe both and do virtual mapping (see `detect()`).
const PROBE_BINARIES: &[&str] = &[
    "python3", "python", "pip3", "pip", "uv", "node", "npm", "git", "curl",
];

impl EnvironmentProbe {
    /// Probe all binaries concurrently. Timeout: 2s per binary.
    pub async fn detect(agent_config: &AgentSection) -> Self {
        let mut names: Vec<String> = PROBE_BINARIES.iter().map(|s| (*s).to_string()).collect();
        for extra in &agent_config.probe_extra_binaries {
            if !names.contains(extra) {
                names.push(extra.clone());
            }
        }

        let handles: Vec<_> = names
            .iter()
            .map(|name| {
                let name = name.clone();
                tokio::spawn(async move { (name.clone(), probe_binary(&name).await) })
            })
            .collect();

        let mut binaries = HashMap::new();
        for handle in handles {
            if let Ok((name, info)) = handle.await {
                binaries.insert(name, info);
            }
        }

        // Windows special handling: if python3 is NOT available but python IS,
        // and python's version is 3.x, create a virtual python3 entry.
        #[cfg(target_os = "windows")]
        {
            let py3_available = binaries
                .get("python3")
                .map(|b| b.available)
                .unwrap_or(false);
            if !py3_available
                && let Some(py) = binaries.get("python")
                && py.available
                && let Some(ref ver) = py.version
                && (ver.starts_with("3.") || ver.starts_with("Python 3."))
            {
                binaries.insert(
                    "python3".to_string(),
                    BinaryInfo {
                        available: true,
                        version: py.version.clone(),
                        path: py.path.clone(),
                    },
                );
            }
        }

        let probe = Self { binaries };
        tracing::info!(
            available = ?probe.binaries.iter()
                .filter(|(_, b)| b.available)
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            "environment probe complete / 环境探测完成"
        );
        probe
    }

    /// Check if a binary is available.
    pub fn has(&self, name: &str) -> bool {
        self.binaries
            .get(name)
            .map(|b| b.available)
            .unwrap_or(false)
    }

    /// Generate the `## Runtime Environment` section for the system prompt.
    pub fn to_prompt_section(&self, agent_config: &AgentSection) -> String {
        let mut s = String::from("## Runtime Environment\n\n");
        s += "The following runtimes are detected on this system:\n\n";

        // Stable ordering for deterministic output
        let mut names: Vec<&String> = self.binaries.keys().collect();
        names.sort();

        for name in &names {
            let info = &self.binaries[*name];
            if info.available {
                let ver = info.version.as_deref().unwrap_or("unknown version");
                s += &format!("- {name}: available ({ver})\n");
            } else {
                s += &format!("- {name}: NOT available\n");
            }
        }

        // Environment guidelines
        s += "\n### Environment Guidelines\n\n";

        let has_python = self.has("python3") || self.has("python");
        let has_pip = self.has("pip3") || self.has("pip");
        let has_uv = self.has("uv");
        let managed_bootstrap =
            agent_config.auto_install_packages && agent_config.install_scope == "venv";

        if has_python && has_pip {
            s += "- Python is available with pip. You can install packages and run scripts.\n";
            s += "- For data processing (xlsx, csv, docx), write Python scripts and execute via shell_exec.\n";
            s += "- Store generated scripts under `script/` in the workspace root, not under `workspace/`.\n";
            s +=
                "- If a package is missing, install it first (prefer uv if available, else pip).\n";
        } else if managed_bootstrap {
            s += "- A managed Python runtime can be bootstrapped automatically for shell_exec tasks.\n";
            s += "- For data processing (xlsx, csv, docx), write Python scripts and execute via shell_exec.\n";
            s += "- Store generated scripts under `script/` in the workspace root, not under `workspace/`.\n";
            s += "- If Python, pip, or uv is missing, shell_exec may prepare the isolated runtime automatically.\n";
        } else if has_python && !has_pip {
            s += "- Python is available but pip is NOT. You can run scripts with stdlib only.\n";
            s += "- If packages are needed, inform the user to install pip first.\n";
        } else {
            s += "- Python is NOT available on this system.\n";
            s += "- For tasks requiring Python, inform the user to install Python.\n";
        }

        if has_uv {
            s += "- uv is available. Prefer `uv pip install` over `pip install` for speed.\n";
            s += "- Use `uv venv` for isolated environments when installing packages.\n";
        } else if managed_bootstrap {
            s += "- uv is not currently available, but the managed runtime bootstrapper may install it automatically.\n";
        }

        // Package installation policy
        s += "\n### Package Installation Policy\n\n";
        if agent_config.auto_install_packages {
            s += "- You ARE allowed to install Python packages when needed.\n";
            if agent_config.install_scope == "venv" {
                let venv = &agent_config.venv_path;
                s += &format!("- ALWAYS use a virtual environment at `{venv}`.\n");
                s += "- shell_exec will prepare the environment automatically before Python-oriented commands when possible.\n";
                s += &format!(
                    "- Managed Python target version: `{}`.\n",
                    agent_config.managed_python_version
                );
                #[cfg(target_os = "windows")]
                {
                    s += &format!(
                        "- Prefer commands like `python script.py` or `uv pip install <pkg>`; activation is not required because the command will be rewritten to `{venv}` automatically.\n"
                    );
                    s += "- shell_exec runs from the workspace root on Windows, so relative paths like `script/task.py` and `设备数据导出.xlsx` should be workspace-relative.\n";
                }
                #[cfg(not(target_os = "windows"))]
                {
                    s += &format!(
                        "- Prefer commands like `python script.py` or `uv pip install <pkg>`; activation is not required because the command will be rewritten to `{venv}` automatically.\n"
                    );
                }
            }
        } else {
            s += "- You are NOT allowed to install packages automatically.\n";
            s += "- If a package is missing, inform the user what to install and how.\n";
        }

        s
    }
}

/// Probe a single binary: check existence via which/where, then get version.
async fn probe_binary(name: &str) -> BinaryInfo {
    let which_cmd = if cfg!(target_os = "windows") {
        "where"
    } else {
        "which"
    };

    let path = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_cmd(which_cmd, &[name]),
    )
    .await
    {
        Ok(Ok(output)) if !output.is_empty() => Some(output),
        _ => None,
    };

    if path.is_none() {
        return BinaryInfo {
            available: false,
            version: None,
            path: None,
        };
    }

    let version = match tokio::time::timeout(
        std::time::Duration::from_secs(2),
        run_cmd(name, &["--version"]),
    )
    .await
    {
        Ok(Ok(output)) if !output.is_empty() => {
            // Extract version string: take first line, trim
            let first_line = output.lines().next().unwrap_or(&output).trim().to_string();
            Some(first_line)
        }
        _ => None,
    };

    BinaryInfo {
        available: true,
        version,
        path: path.map(|p| p.lines().next().unwrap_or("").trim().to_string()),
    }
}

/// Run a command and capture stdout (merged with stderr for version commands).
async fn run_cmd(program: &str, args: &[&str]) -> std::io::Result<String> {
    let output = tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        // Some tools (e.g. python --version on older versions) write to stderr
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        if !stderr.is_empty() {
            Ok(stderr)
        } else {
            Ok(String::new())
        }
    }
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn make_probe(entries: &[(&str, bool, Option<&str>)]) -> EnvironmentProbe {
        let mut binaries = HashMap::new();
        for (name, available, version) in entries {
            binaries.insert(
                name.to_string(),
                BinaryInfo {
                    available: *available,
                    version: version.map(|v| v.to_string()),
                    path: if *available {
                        Some(format!("/usr/bin/{name}"))
                    } else {
                        None
                    },
                },
            );
        }
        EnvironmentProbe { binaries }
    }

    fn default_agent_section() -> AgentSection {
        AgentSection::default()
    }

    #[test]
    fn test_to_prompt_section_with_python() {
        let probe = make_probe(&[
            ("python3", true, Some("Python 3.11.5")),
            ("pip3", true, Some("pip 23.2.1")),
            ("node", false, None),
            ("git", true, Some("git version 2.42.0")),
        ]);
        let section = probe.to_prompt_section(&default_agent_section());
        assert!(section.contains("## Runtime Environment"));
        assert!(section.contains("python3: available (Python 3.11.5)"));
        assert!(section.contains("pip3: available"));
        assert!(section.contains("node: NOT available"));
        assert!(section.contains("Python is available with pip"));
        assert!(section.contains("write Python scripts"));
    }

    #[test]
    fn test_to_prompt_section_without_python() {
        let probe = make_probe(&[
            ("python3", false, None),
            ("python", false, None),
            ("pip3", false, None),
            ("pip", false, None),
            ("node", true, Some("v20.10.0")),
        ]);
        let section = probe.to_prompt_section(&default_agent_section());
        assert!(section.contains("Python is NOT available"));
        assert!(section.contains("inform the user to install Python"));
    }

    #[test]
    fn test_to_prompt_section_python_without_pip() {
        let probe = make_probe(&[
            ("python3", true, Some("Python 3.11.5")),
            ("pip3", false, None),
            ("pip", false, None),
        ]);
        let section = probe.to_prompt_section(&default_agent_section());
        assert!(section.contains("pip is NOT"));
        assert!(section.contains("stdlib only"));
    }

    #[test]
    fn test_to_prompt_section_install_policy_enabled() {
        let probe = make_probe(&[
            ("python3", true, Some("Python 3.11.5")),
            ("pip3", true, Some("pip 23.2.1")),
            ("uv", true, Some("uv 0.1.0")),
        ]);
        let mut config = default_agent_section();
        config.auto_install_packages = true;
        config.install_scope = "venv".to_string();
        config.venv_path = "workspace/.venv".to_string();
        config.managed_python_version = "3.12".to_string();

        let section = probe.to_prompt_section(&config);
        assert!(section.contains("You ARE allowed to install"));
        assert!(section.contains("virtual environment"));
        assert!(section.contains(".venv"));
        assert!(section.contains("Managed Python target version"));
    }

    #[test]
    fn test_to_prompt_section_install_policy_disabled() {
        let probe = make_probe(&[
            ("python3", true, Some("Python 3.11.5")),
            ("pip3", true, Some("pip 23.2.1")),
        ]);
        let config = default_agent_section(); // auto_install_packages defaults to false

        let section = probe.to_prompt_section(&config);
        assert!(section.contains("You are NOT allowed to install"));
        assert!(section.contains("inform the user"));
    }

    #[test]
    fn test_to_prompt_section_uv_available() {
        let probe = make_probe(&[
            ("python3", true, Some("Python 3.11.5")),
            ("pip3", true, Some("pip 23.2.1")),
            ("uv", true, Some("uv 0.5.0")),
        ]);
        let section = probe.to_prompt_section(&default_agent_section());
        assert!(section.contains("uv is available"));
        assert!(section.contains("Prefer `uv pip install`"));
    }

    #[test]
    fn test_to_prompt_section_bootstraps_when_python_missing() {
        let probe = make_probe(&[
            ("python3", false, None),
            ("python", false, None),
            ("uv", false, None),
        ]);
        let mut config = default_agent_section();
        config.auto_install_packages = true;
        config.install_scope = "venv".to_string();
        let section = probe.to_prompt_section(&config);
        assert!(section.contains("bootstrapped automatically"));
        assert!(section.contains("shell_exec may prepare the isolated runtime automatically"));
    }

    #[test]
    fn test_has_method() {
        let probe = make_probe(&[
            ("python3", true, Some("Python 3.11.5")),
            ("node", false, None),
        ]);
        assert!(probe.has("python3"));
        assert!(!probe.has("node"));
        assert!(!probe.has("nonexistent"));
    }
}
