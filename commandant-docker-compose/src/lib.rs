pub mod docker_paths;
pub mod error;
pub mod executor;
pub mod model;
pub mod parser;
pub mod planner;
pub mod proxy;

pub use error::{Error, Result};
pub use executor::{ComposeExecutor, RunningProject};
pub use model::{ComposeProject, PortMapping};
pub use planner::{
    ExecutionPlan, MountKind, NetworkPlan, ResolvedHealthcheck, ResolvedMount, ResolvedPort,
    RestartPolicy, ServiceNetworkAttachment, ServicePlan, VolumePlan,
};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::panic::{self, AssertUnwindSafe};
    use std::path::{Path, PathBuf};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use anyhow::{Context, Result as AnyhowResult, bail};
    use bollard::exec::{StartExecOptions, StartExecResults};
    use futures_util::{FutureExt, StreamExt};

    use super::*;

    const FIXTURE: &str = r#"
services:
  api:
    image: nginx:latest
    ports:
      - "8080:80"
    environment:
      APP_ENV: test
    networks:
      app:
        aliases:
          - api.local
    volumes:
      - app-data:/data
      - ./config:/etc/nginx:ro
    depends_on:
      - db

  db:
    image: postgres:16
    profiles:
      - database

networks:
  app:

volumes:
  app-data:
"#;

    #[test]
    fn project_is_mutable() {
        let mut project =
            ComposeProject::from_yaml_strl_str(FIXTURE).expect("parse compose fixture");

        project
            .set_env("api", "LOG_LEVEL", "debug")
            .expect("set env");
        project
            .add_port("api", PortMapping::tcp(8443, 443))
            .expect("add port");
        project
            .set_network_aliases("api", "app", vec!["api.internal".to_string()])
            .expect("set aliases");

        let service = project.service("api").expect("service exists");
        let env = service.environment.as_ref().expect("environment exists");
        let ports = service.ports.as_ref().expect("ports exist");
        let networks = service.networks.as_ref().expect("networks exist");

        match env {
            docker_compose_spec::VecOrMap::Map(values) => {
                assert!(values.contains_key(&docker_compose_spec::NonEmptyKey::from("LOG_LEVEL")));
            }
            _ => panic!("expected mapped environment"),
        }

        assert_eq!(ports.len(), 2);

        match networks {
            docker_compose_spec::ServiceNetworks::Map(values) => {
                assert_eq!(
                    values
                        .get(&docker_compose_spec::FieldKey::from("app"))
                        .and_then(|network| network.aliases.as_ref())
                        .expect("aliases exist"),
                    &vec!["api.internal".to_string()]
                );
            }
            _ => panic!("expected mapped networks"),
        }
    }

    #[test]
    fn planner_resolves_runtime_configuration() {
        let project = ComposeProject::from_yaml_strl_str(FIXTURE).expect("parse compose fixture");
        let plan = planner::build_plan(&project, &["database".to_string()]).expect("build plan");

        assert_eq!(plan.services.len(), 3);
        assert_eq!(plan.networks.len(), 2);
        assert_eq!(plan.volumes.len(), 1);

        let api = plan.service("api").expect("api plan exists");
        assert_eq!(api.ports.len(), 1);
        assert_eq!(api.mounts.len(), 2);
        assert_eq!(api.networks[0].runtime_name, "app");
        assert_eq!(api.depends_on, vec!["db".to_string()]);

        let proxy = plan.service("__commandant_traefik").expect("proxy plan exists");
        assert_eq!(proxy.ports[0].host_port, 80);
        assert_eq!(proxy.networks[0].runtime_name, plan.proxy.as_ref().expect("proxy session").network_runtime_name);
        assert_eq!(proxy.container_name, "commandant-traefik");
    }

    #[test]
    fn planner_prefixes_implicit_names_with_parent_directory() {
        let project = ComposeProject::from_path(
            "/Users/mac-ESELLI02/Documents/PERSO/Commandant/commandant-docker-compose/fixtures/sample/compose.yml",
        )
        .unwrap_or_else(|_| {
            ComposeProject::new(
                FIXTURE.parse().expect("parse compose fixture"),
                "/tmp/example-project",
            )
        });

        let plan = planner::build_plan(&project, &["database".to_string()]).expect("build plan");
        let api = plan.service("api").expect("api plan exists");

        assert_eq!(api.container_name, "example-project_api");
        assert_eq!(api.networks[0].runtime_name, "example-project_app");
        assert_eq!(plan.volumes[0].runtime_name, "example-project_app-data");
        assert!(
            api.mounts
                .iter()
                .any(|mount| mount.source == "example-project_app-data")
        );
    }

    #[test]
    fn planner_merges_dotenv_env_files_and_service_environment() {
        let test_dir = TestDir::new("commandant-compose-env").expect("create temp dir");
        fs::write(
            test_dir.path().join(".env"),
            "FROM_DOTENV=dotenv\nSHARED=dotenv\n",
        )
        .expect("write dotenv");
        fs::write(
            test_dir.path().join("service.env"),
            "FROM_ENV_FILE=env-file\nSHARED=env-file\n",
        )
        .expect("write env file");
        fs::write(
            test_dir.path().join("compose.yml"),
            "services:\n  app:\n    image: alpine:3.19\n    env_file:\n      - ./service.env\n    environment:\n      SHARED: explicit\n      EXPLICIT_ONLY: set\n",
        )
        .expect("write compose file");

        let project = ComposeProject::from_path(test_dir.path().join("compose.yml"))
            .expect("parse compose file");
        let plan = planner::build_plan(&project, &[]).expect("build plan");
        let service = plan.service("app").expect("service exists");

        assert_eq!(
            service.environment.get("FROM_DOTENV").map(String::as_str),
            Some("dotenv")
        );
        assert_eq!(
            service.environment.get("FROM_ENV_FILE").map(String::as_str),
            Some("env-file")
        );
        assert_eq!(
            service.environment.get("SHARED").map(String::as_str),
            Some("explicit")
        );
        assert_eq!(
            service.environment.get("EXPLICIT_ONLY").map(String::as_str),
            Some("set")
        );
    }

    #[test]
    fn planner_supports_target_only_anonymous_volumes() {
        let project = ComposeProject::new(
            "services:\n  app:\n    image: alpine:3.19\n    volumes:\n      - /node_modules\n"
                .parse()
                .expect("parse compose fixture"),
            "/tmp/example-project",
        );

        let plan = planner::build_plan(&project, &[]).expect("build plan");
        let service = plan.service("app").expect("service exists");
        let mount = service.mounts.first().expect("mount exists");

        assert_eq!(mount.kind, MountKind::Volume);
        assert!(mount.anonymous);
        assert_eq!(mount.source, "");
        assert_eq!(mount.target, "/node_modules");
        assert!(mount.compose_source.is_none());
        assert!(plan.volumes.is_empty());
    }

    #[test]
    fn planner_resolves_healthcheck_configuration() {
        let project = ComposeProject::from_yaml_strl_str(
            r#"
services:
  app:
    image: alpine:3.19
    healthcheck:
      test: ["CMD-SHELL", "test -f /tmp/healthy"]
      interval: 5s
      timeout: 2s
      retries: 3
      start_period: 7s
      start_interval: 1s
"#,
        )
        .expect("parse compose fixture");

        let plan = planner::build_plan(&project, &[]).expect("build plan");
        let healthcheck = plan
            .service("app")
            .and_then(|service| service.healthcheck.as_ref())
            .expect("healthcheck exists");

        assert_eq!(
            healthcheck.test.as_ref().expect("test exists"),
            &vec!["CMD-SHELL".to_string(), "test -f /tmp/healthy".to_string()]
        );
        assert_eq!(healthcheck.interval_ns, Some(5_000_000_000));
        assert_eq!(healthcheck.timeout_ns, Some(2_000_000_000));
        assert_eq!(healthcheck.retries, Some(3));
        assert_eq!(healthcheck.start_period_ns, Some(7_000_000_000));
        assert_eq!(healthcheck.start_interval_ns, Some(1_000_000_000));
    }

    #[test]
    fn planner_supports_disabled_healthcheck() {
        let project = ComposeProject::from_yaml_strl_str(
            r#"
services:
  app:
    image: alpine:3.19
    healthcheck:
      disable: true
"#,
        )
        .expect("parse compose fixture");

        let plan = planner::build_plan(&project, &[]).expect("build plan");
        let healthcheck = plan
            .service("app")
            .and_then(|service| service.healthcheck.as_ref())
            .expect("healthcheck exists");

        assert_eq!(healthcheck.test, Some(vec!["NONE".to_string()]));
        assert_eq!(healthcheck.interval_ns, None);
    }

    #[tokio::test]
    async fn executor_applies_runtime_config_in_container() -> AnyhowResult<()> {
        const IMAGE: &str = "alpine:3.19";

        let Ok(executor) = ComposeExecutor::new() else {
            eprintln!("skipping live Docker test: failed to create Docker client");
            return Ok(());
        };

        if let Err(error) = executor.client().ping().await {
            eprintln!("skipping live Docker test: Docker daemon unavailable: {error}");
            return Ok(());
        }

        if executor
            .client()
            .images()
            .get(IMAGE)
            .inspect()
            .await
            .is_err()
            && executor.client().images().pull(IMAGE, None).await.is_err()
        {
            eprintln!("skipping live Docker test: unable to ensure image {IMAGE}");
            return Ok(());
        }

        let test_dir = TestDir::new("commandant-compose-live")?;
        let fixture_dir = test_dir.path().join("fixture");
        fs::create_dir_all(&fixture_dir)?;
        fs::write(fixture_dir.join("config.txt"), "mounted-from-host\n")?;

        let container_name = unique_name("commandant-compose-live");
        let compose_path = test_dir.path().join("compose.yml");
        fs::write(
            &compose_path,
            format!(
                "services:\n  probe:\n    image: {IMAGE}\n    container_name: {container_name}\n    command: [\"sh\", \"-c\", \"while true; do sleep 1; done\"]\n    working_dir: /workspace\n    environment:\n      APP_ENV: integration\n      CONFIG_PATH: /workspace/fixture/config.txt\n    volumes:\n      - ./fixture:/workspace/fixture:ro\n"
            ),
        )?;

        let project = ComposeProject::from_path(&compose_path)?;
        let running = executor.up(&project, &[]).await?;
        let container_id = running
            .containers
            .get("probe")
            .context("missing probe container")?
            .id()
            .to_string();

        let test_result = AssertUnwindSafe(async {
            let output = exec_stdout(
                executor.client(),
                &container_id,
                vec![
                    "sh",
                    "-lc",
                    "printf '%s\\n%s\\n' \"$APP_ENV\" \"$CONFIG_PATH\" && pwd && cat \"$CONFIG_PATH\"",
                ],
            )
            .await
            .expect("exec probe command");

            let lines = output.lines().collect::<Vec<_>>();
            assert_eq!(lines, vec!["integration", "/workspace/fixture/config.txt", "/workspace", "mounted-from-host"]);
        })
        .catch_unwind()
        .await;

        let down_result = executor.down(running).await;
        if let Err(payload) = test_result {
            let _ = down_result;
            panic::resume_unwind(payload);
        }
        down_result?;

        Ok(())
    }

    #[tokio::test]
    async fn executor_injects_dotenv_and_env_file_values() -> AnyhowResult<()> {
        const IMAGE: &str = "alpine:3.19";

        let Ok(executor) = ComposeExecutor::new() else {
            eprintln!("skipping live Docker test: failed to create Docker client");
            return Ok(());
        };

        if let Err(error) = executor.client().ping().await {
            eprintln!("skipping live Docker test: Docker daemon unavailable: {error}");
            return Ok(());
        }

        if executor
            .client()
            .images()
            .get(IMAGE)
            .inspect()
            .await
            .is_err()
            && executor.client().images().pull(IMAGE, None).await.is_err()
        {
            eprintln!("skipping live Docker test: unable to ensure image {IMAGE}");
            return Ok(());
        }

        let test_dir = TestDir::new("commandant-compose-env-live")?;
        fs::write(
            test_dir.path().join(".env"),
            "FROM_DOTENV=dotenv\nSHARED_VALUE=dotenv\n",
        )?;
        fs::write(
            test_dir.path().join("service.env"),
            "FROM_ENV_FILE=env-file\nSHARED_VALUE=env-file\n",
        )?;

        let container_name = unique_name("commandant-compose-env-live");
        let compose_path = test_dir.path().join("compose.yml");
        fs::write(
            &compose_path,
            format!(
                "services:\n  probe:\n    image: {IMAGE}\n    container_name: {container_name}\n    command: [\"sh\", \"-c\", \"while true; do sleep 1; done\"]\n    env_file:\n      - ./service.env\n    environment:\n      SHARED_VALUE: explicit\n      EXPLICIT_ONLY: explicit-only\n"
            ),
        )?;

        let project = ComposeProject::from_path(&compose_path)?;
        let running = executor.up(&project, &[]).await?;
        let container_id = running
            .containers
            .get("probe")
            .context("missing probe container")?
            .id()
            .to_string();

        let test_result = AssertUnwindSafe(async {
            let output = exec_stdout(
                executor.client(),
                &container_id,
                vec![
                    "sh",
                    "-lc",
                    "printf '%s\\n%s\\n%s\\n%s\\n' \"$FROM_DOTENV\" \"$FROM_ENV_FILE\" \"$SHARED_VALUE\" \"$EXPLICIT_ONLY\"",
                ],
            )
            .await
            .expect("exec env probe command");

            let lines = output.lines().collect::<Vec<_>>();
            assert_eq!(
                lines,
                vec!["dotenv", "env-file", "explicit", "explicit-only"]
            );
        })
        .catch_unwind()
        .await;

        let down_result = executor.down(running).await;
        if let Err(payload) = test_result {
            let _ = down_result;
            panic::resume_unwind(payload);
        }
        down_result?;

        Ok(())
    }

    #[tokio::test]
    async fn executor_creates_target_only_mount_as_anonymous_volume() -> AnyhowResult<()> {
        const IMAGE: &str = "alpine:3.19";

        let Ok(executor) = ComposeExecutor::new() else {
            eprintln!("skipping live Docker test: failed to create Docker client");
            return Ok(());
        };

        if let Err(error) = executor.client().ping().await {
            eprintln!("skipping live Docker test: Docker daemon unavailable: {error}");
            return Ok(());
        }

        if executor
            .client()
            .images()
            .get(IMAGE)
            .inspect()
            .await
            .is_err()
            && executor.client().images().pull(IMAGE, None).await.is_err()
        {
            eprintln!("skipping live Docker test: unable to ensure image {IMAGE}");
            return Ok(());
        }

        let test_dir = TestDir::new("commandant-compose-anon-volume")?;
        let container_name = unique_name("commandant-compose-anon-volume");
        let compose_path = test_dir.path().join("compose.yml");
        fs::write(
            &compose_path,
            format!(
                "services:\n  probe:\n    image: {IMAGE}\n    container_name: {container_name}\n    command: [\"sh\", \"-c\", \"while true; do sleep 1; done\"]\n    volumes:\n      - /node_modules\n"
            ),
        )?;

        let project = ComposeProject::from_path(&compose_path)?;
        let running = executor.up(&project, &[]).await?;
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

        let test_result = AssertUnwindSafe(async move {
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
                    .name
                    .as_deref()
                    .is_some_and(|name| !name.is_empty())
            );
            assert!(
                node_modules
                    .source
                    .as_deref()
                    .is_some_and(|source| !source.is_empty())
            );
        })
        .catch_unwind()
        .await;

        let down_result = executor.down(running).await;
        if let Err(payload) = test_result {
            let _ = down_result;
            panic::resume_unwind(payload);
        }
        down_result?;

        Ok(())
    }

    async fn exec_stdout(
        client: &lmrc_docker::DockerClient,
        container_id: &str,
        cmd: Vec<&str>,
    ) -> AnyhowResult<String> {
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

    fn unique_name(prefix: &str) -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("{prefix}-{}-{nanos}", std::process::id())
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(prefix: &str) -> AnyhowResult<Self> {
            let path = std::env::temp_dir().join(unique_name(prefix));
            fs::create_dir_all(&path)?;
            Ok(Self { path })
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}
