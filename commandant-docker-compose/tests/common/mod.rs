use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Result, bail};
use bollard::exec::{StartExecOptions, StartExecResults};
use commandant_docker_compose::docker_paths;
use futures_util::StreamExt;
use lmrc_docker::DockerClient;

pub fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

pub async fn exec_stdout(
    client: &DockerClient,
    container_id: &str,
    cmd: Vec<&str>,
) -> Result<String> {
    let exec_id = client
        .containers()
        .get(container_id)
        .exec(cmd.into_iter().map(str::to_string).collect(), false)
        .await?;

    let mut output_text = String::new();
    match client
        .inner()
        .start_exec(&exec_id, None::<StartExecOptions>)
        .await?
    {
        StartExecResults::Attached { mut output, .. } => {
            while let Some(chunk) = output.next().await {
                output_text.push_str(&chunk?.to_string());
            }
        }
        StartExecResults::Detached => bail!("exec unexpectedly detached"),
    }

    for _ in 0..20 {
        let inspect = client.inner().inspect_exec(&exec_id).await?;
        match inspect.exit_code {
            Some(0) => return Ok(output_text),
            Some(code) => bail!("exec failed with exit code {code}: {output_text}"),
            None => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }

    bail!("timed out waiting for exec completion")
}

pub async fn network_exists(client: &DockerClient, name: &str) -> Result<bool> {
    Ok(client
        .networks()
        .list()
        .await?
        .into_iter()
        .any(|network| network.name.as_deref() == Some(name)))
}

pub async fn volume_exists(client: &DockerClient, name: &str) -> Result<bool> {
    Ok(client
        .volumes()
        .list()
        .await?
        .into_iter()
        .any(|volume| volume.name == name))
}

pub fn docker_client() -> Result<DockerClient> {
    docker_paths::configure_docker_host();
    Ok(DockerClient::new()?)
}
