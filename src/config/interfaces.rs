use anyhow::{Result, ensure};
use serde::Deserialize;

/// Each service owns its own native runtime and interface list.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReticulumInterface {
    TcpClient {
        host: String,
        port: u16,
    },
    TcpServer {
        listen: String,
        port: u16,
    },
    Auto {
        #[serde(default = "group")]
        group_id: String,
        #[serde(default = "discovery_port")]
        discovery_port: u16,
        #[serde(default = "data_port")]
        data_port: u16,
        #[serde(default)]
        devices: Vec<String>,
    },
}
fn group() -> String {
    "reticulum".into()
}
fn discovery_port() -> u16 {
    29716
}
fn data_port() -> u16 {
    42671
}

fn value(value: &str) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && !value.chars().any(|c| c.is_control() || c == '"'),
        "interface values must be nonempty, without control characters or double quotes"
    );
    Ok(())
}

impl ReticulumInterface {
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::TcpClient { host, port } => {
                value(host)?;
                ensure!(*port > 0, "TCP port must be nonzero");
            }
            Self::TcpServer { listen, port } => {
                value(listen)?;
                ensure!(*port > 0, "TCP port must be nonzero");
            }
            Self::Auto {
                group_id,
                discovery_port,
                data_port,
                devices,
            } => {
                value(group_id)?;
                ensure!(
                    *discovery_port > 0 && *data_port > 0 && discovery_port != data_port,
                    "AutoInterface ports must be nonzero and distinct"
                );
                for device in devices {
                    value(device)?;
                    ensure!(!device.contains(','), "device names cannot contain commas");
                }
            }
        }
        Ok(())
    }

    pub fn render(&self, index: usize) -> String {
        let mut text = format!("\n[[Interface {index}]]\nenabled = Yes\n");
        match self {
            Self::TcpClient { host, port } => text.push_str(&format!(
                "type = TCPClientInterface\ntarget_host = \"{host}\"\ntarget_port = {port}\n"
            )),
            Self::TcpServer { listen, port } => text.push_str(&format!(
                "type = TCPServerInterface\nlisten_ip = \"{listen}\"\nlisten_port = {port}\n"
            )),
            Self::Auto {
                group_id,
                discovery_port,
                data_port,
                devices,
            } => {
                text.push_str(&format!("type = AutoInterface\ngroup_id = \"{group_id}\"\ndiscovery_port = {discovery_port}\ndata_port = {data_port}\n"));
                if !devices.is_empty() {
                    text.push_str(&format!("devices = \"{}\"\n", devices.join(",")));
                }
            }
        }
        text
    }
}
