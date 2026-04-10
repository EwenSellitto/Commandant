mod common;

use anyhow::{Context, Result as AnyhowResult};
use tempfile::TempDir;

use commandant_docker_compose::{ComposeExecutor, ComposeProject, proxy::ProxyConfig};

use common::{exec_stdout, unique_name};

const IMAGE: &str = "alpine:3.19";

#[tokio::test]
async fn executor_applies_runtime_config_in_container() -> AnyhowResult<()> {
    let executor = ensure_executor().await?;
    ensure_image(&executor, IMAGE).await?;

    let test_dir = TempDir::new()?;
    let fixture_dir = test_dir.path().join("fixture");
    std::fs::create_dir_all(&fixture_dir)?;
    std::fs::write(fixture_dir.join("config.txt"), "mounted-from-host\n")?;

    let container_name = unique_name("commandant-compose-live");
    let compose_path = test_dir.path().join("compose.yml");
    std::fs::write(
        &compose_path,
        format!(
            "services:\n  probe:\n    image: {IMAGE}\n    container_name: {container_name}\n    command: [\"sh\", \"-c\", \"while true; do sleep 1; done\"]\n    working_dir: /workspace\n    environment:\n      APP_ENV: integration\n      CONFIG_PATH: /workspace/fixture/config.txt\n    volumes:\n      - ./fixture:/workspace/fixture:ro\n"
        ),
    )?;

    let project = ComposeProject::from_path(&compose_path)?;
    let running = executor
        .up_with_proxy(
            &project,
            &[],
            &ProxyConfig {
                host_port: 18081,
                ..ProxyConfig::default()
            },
        )
        .await?;
    let container_id = running
        .containers
        .get("probe")
        .context("missing probe container")?
        .id()
        .to_string();

    let output = exec_stdout(
        executor.client(),
        &container_id,
        vec![
            "sh",
            "-lc",
            "printf '%s\n%s\n' \"$APP_ENV\" \"$CONFIG_PATH\" && pwd && cat \"$CONFIG_PATH\"",
        ],
    )
    .await?;

    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        vec![
            "integration",
            "/workspace/fixture/config.txt",
            "/workspace",
            "mounted-from-host"
        ]
    );

    executor.down(running).await?;
    Ok(())
}

#[tokio::test]
async fn executor_injects_dotenv_and_env_file_values() -> AnyhowResult<()> {
    let executor = ensure_executor().await?;
    ensure_image(&executor, IMAGE).await?;

    let test_dir = TempDir::new()?;
    std::fs::write(
        test_dir.path().join(".env"),
        "FROM_DOTENV=dotenv\nSHARED_VALUE=dotenv\n",
    )?;
    std::fs::write(
        test_dir.path().join("service.env"),
        "FROM_ENV_FILE=env-file\nSHARED_VALUE=env-file\n",
    )?;

    let container_name = unique_name("commandant-compose-env-live");
    let compose_path = test_dir.path().join("compose.yml");
    std::fs::write(
        &compose_path,
        format!(
            "services:\n  probe:\n    image: {IMAGE}\n    container_name: {container_name}\n    command: [\"sh\", \"-c\", \"while true; do sleep 1; done\"]\n    env_file:\n      - ./service.env\n    environment:\n      SHARED_VALUE: explicit\n      EXPLICIT_ONLY: explicit-only\n"
        ),
    )?;

    let project = ComposeProject::from_path(&compose_path)?;
    let running = executor
        .up_with_proxy(
            &project,
            &[],
            &ProxyConfig {
                host_port: 18082,
                ..ProxyConfig::default()
            },
        )
        .await?;
    let container_id = running
        .containers
        .get("probe")
        .context("missing probe container")?
        .id()
        .to_string();

    let output = exec_stdout(
        executor.client(),
        &container_id,
        vec![
            "sh",
            "-lc",
            "printf '%s\n%s\n%s\n%s\n' \"$FROM_DOTENV\" \"$FROM_ENV_FILE\" \"$SHARED_VALUE\" \"$EXPLICIT_ONLY\"",
        ],
    )
    .await?;

    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        vec!["dotenv", "env-file", "explicit", "explicit-only"]
    );

    executor.down(running).await?;
    Ok(())
}

#[tokio::test]
async fn executor_creates_target_only_mount_as_anonymous_volume() -> AnyhowResult<()> {
    let executor = ensure_executor().await?;
    ensure_image(&executor, IMAGE).await?;

    let test_dir = TempDir::new()?;
    let container_name = unique_name("commandant-compose-anon-volume");
    let compose_path = test_dir.path().join("compose.yml");
    std::fs::write(
        &compose_path,
        format!(
            "services:\n  probe:\n    image: {IMAGE}\n    container_name: {container_name}\n    command: [\"sh\", \"-c\", \"while true; do sleep 1; done\"]\n    volumes:\n      - /node_modules\n"
        ),
    )?;

    let project = ComposeProject::from_path(&compose_path)?;
    let running = executor
        .up_with_proxy(
            &project,
            &[],
            &ProxyConfig {
                host_port: 18083,
                ..ProxyConfig::default()
            },
        )
        .await?;
    let container_id = running
        .containers
        .get("probe")
        .context("missing probe container")?
        .id()
        .to_string();

    let inspect = executor
        .client()
        .inner()
        .inspect_container(
            &container_id,
            None::<bollard::query_parameters::InspectContainerOptions>,
        )
        .await?;

    let mounts = inspect.mounts.expect("mounts exist");
    let node_modules = mounts
        .into_iter()
        .find(|mount| mount.destination.as_deref() == Some("/node_modules"))
        .expect("anonymous node_modules mount exists");

    assert_eq!(
        node_modules.typ.map(|typ| typ.to_string()),
        Some("volume".to_string())
    );
    assert!(
        node_modules
            .source
            .as_deref()
            .is_some_and(|source| !source.is_empty())
    );

    executor.down(running).await?;
    Ok(())
}

async fn ensure_executor() -> AnyhowResult<ComposeExecutor> {
    let executor = ComposeExecutor::new()?;
    executor.client().ping().await?;
    Ok(executor)
}

async fn ensure_image(executor: &ComposeExecutor, image: &str) -> AnyhowResult<()> {
    if executor
        .client()
        .images()
        .get(image)
        .inspect()
        .await
        .is_ok()
    {
        return Ok(());
    }

    executor.client().images().pull(image, None).await?;
    Ok(())
}
