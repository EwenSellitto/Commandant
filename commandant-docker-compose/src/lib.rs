mod naming;
mod planner_exposure;
mod planner_flatten;
mod planner_runtime;
mod proxy_labels;

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
    RestartPolicy, ServiceExposure, ServiceNetworkAttachment, ServicePlan, TagSource, VolumePlan,
};

#[cfg(test)]
mod tests {
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
        let mut project = ComposeProject::from_yaml_str(FIXTURE).expect("parse compose fixture");

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
        let project = ComposeProject::from_yaml_str(FIXTURE).expect("parse compose fixture");
        let plan = planner::build_plan_with_proxy(
            &project,
            &["database".to_string()],
            &proxy::ProxyConfig {
                host_port: 18080,
                ..proxy::ProxyConfig::default()
            },
        )
        .expect("build plan");

        assert_eq!(plan.services.len(), 3);
        assert_eq!(plan.networks.len(), 2);
        assert_eq!(plan.volumes.len(), 1);

        let api = plan.service("api").expect("api plan exists");
        assert!(api.ports.is_empty());
        assert_eq!(api.mounts.len(), 2);
        assert!(api.networks[0].runtime_name.starts_with("commandant_app_"));
        assert_eq!(api.depends_on, vec!["db".to_string()]);

        let proxy = plan
            .service("__commandant_traefik")
            .expect("proxy plan exists");
        assert_eq!(proxy.ports[0].host_port, 18080);
        assert_eq!(proxy.mounts[0].source, "/var/run/docker.sock");
        assert_eq!(proxy.mounts[0].target, "/var/run/docker.sock");
        assert_eq!(
            proxy.networks[0].runtime_name,
            plan.proxy
                .as_ref()
                .expect("proxy session")
                .network_runtime_name
        );
        assert!(proxy.container_name.starts_with("commandant-traefik-"));
    }

    #[test]
    fn planner_prefixes_implicit_names_with_parent_directory() {
        let test_dir = std::env::current_dir()
            .expect("current dir")
            .join("example-project");
        std::fs::create_dir_all(&test_dir).expect("create temp project dir");
        std::fs::write(test_dir.join("compose.yml"), FIXTURE).expect("write compose file");
        let project =
            ComposeProject::from_path(test_dir.join("compose.yml")).expect("parse compose file");

        let plan = planner::build_plan(&project, &["database".to_string()]).expect("build plan");
        let api = plan.service("api").expect("api plan exists");

        assert_eq!(api.container_name, "example-project_api");
        assert!(
            api.networks[0]
                .runtime_name
                .starts_with("example-project_commandant_app_")
        );
        assert_eq!(plan.volumes[0].runtime_name, "example-project_app-data");
        assert!(
            api.mounts
                .iter()
                .any(|mount| mount.source == "example-project_app-data")
        );

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn planner_merges_dotenv_env_files_and_service_environment() {
        let test_dir = temp_dir("commandant-compose-env");
        std::fs::write(test_dir.join(".env"), "FROM_DOTENV=dotenv\nSHARED=dotenv\n")
            .expect("write dotenv");
        std::fs::write(
            test_dir.join("service.env"),
            "FROM_ENV_FILE=env-file\nSHARED=env-file\n",
        )
        .expect("write env file");
        std::fs::write(
            test_dir.join("compose.yml"),
            "services:\n  app:\n    image: alpine:3.19\n    env_file:\n      - ./service.env\n    environment:\n      SHARED: explicit\n      EXPLICIT_ONLY: set\n",
        )
        .expect("write compose file");

        let project =
            ComposeProject::from_path(test_dir.join("compose.yml")).expect("parse compose file");
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

        let _ = std::fs::remove_dir_all(&test_dir);
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
    fn planner_resolves_bind_mounts_to_absolute_paths_for_relative_compose_files() {
        let workspace_root = std::env::current_dir().expect("current dir");
        let temp_root = workspace_root.join("target");
        std::fs::create_dir_all(&temp_root).expect("create temp root");
        let test_dir = temp_dir_in(&temp_root, "commandant-compose-bind");
        std::fs::write(test_dir.join("redis.conf"), "save 60 1\n").expect("write redis config");
        std::fs::write(
            test_dir.join("compose.yml"),
            "services:\n  redis:\n    image: redis:7-alpine\n    volumes:\n      - ./redis.conf:/usr/local/etc/redis/redis.conf\n",
        )
        .expect("write compose file");

        let relative_path = test_dir
            .strip_prefix(&workspace_root)
            .expect("temp dir inside workspace")
            .join("compose.yml");
        let project = ComposeProject::from_path(&relative_path).expect("parse compose file");
        let plan = planner::build_plan(&project, &[]).expect("build plan");
        let service = plan.service("redis").expect("service exists");
        let mount = service.mounts.first().expect("mount exists");

        assert_eq!(mount.kind, MountKind::Bind);
        assert!(std::path::Path::new(&mount.source).is_absolute());
        assert_eq!(
            std::fs::canonicalize(&mount.source).expect("canonicalize actual mount source"),
            std::fs::canonicalize(test_dir.join("redis.conf"))
                .expect("canonicalize expected mount source")
        );

        let _ = std::fs::remove_dir_all(&test_dir);
    }

    #[test]
    fn planner_resolves_healthcheck_configuration() {
        let project = ComposeProject::from_yaml_str(
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
        let project = ComposeProject::from_yaml_str(
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

    #[test]
    fn planner_resolves_exposed_service_tags() {
        let project = ComposeProject::from_yaml_str(
            r#"
services:
  web:
    image: nginx:latest
    ports:
      - "8080:80"
    labels:
      com.commandant.expose: "true"
      com.commandant.tag: "checkout-web"
"#,
        )
        .expect("parse compose fixture");

        let plan = planner::build_plan_with_proxy(&project, &[], &proxy::ProxyConfig::default())
            .expect("build plan");
        let service = plan.service("web").expect("service exists");

        assert_eq!(
            service
                .exposure
                .as_ref()
                .map(|exposure| exposure.tag.as_str()),
            Some("checkout-web")
        );
        assert_eq!(
            service
                .exposure
                .as_ref()
                .map(|exposure| exposure.hostname.as_str()),
            Some("checkout-web.localhost")
        );
        assert_eq!(
            service
                .exposure
                .as_ref()
                .map(|exposure| exposure.backend_port),
            Some(80)
        );
        assert!(service.ports.is_empty());
    }

    #[test]
    fn planner_auto_exposes_services_with_published_ports() {
        let project = ComposeProject::from_yaml_str(
            r#"
services:
  web:
    image: nginx:latest
    ports:
      - "3000:3000"
"#,
        )
        .expect("parse compose fixture");

        let plan = planner::build_plan_with_proxy(&project, &[], &proxy::ProxyConfig::default())
            .expect("build plan");
        let service = plan.service("web").expect("service exists");

        assert_eq!(
            service
                .exposure
                .as_ref()
                .map(|exposure| exposure.backend_port),
            Some(3000)
        );
        assert!(service.ports.is_empty());
    }

    #[test]
    fn planner_auto_exposes_services_with_native_expose() {
        let project = ComposeProject::from_yaml_str(
            r#"
services:
  web:
    image: nginx:latest
    expose:
      - "3000"
"#,
        )
        .expect("parse compose fixture");

        let plan = planner::build_plan_with_proxy(&project, &[], &proxy::ProxyConfig::default())
            .expect("build plan");
        let service = plan.service("web").expect("service exists");

        assert_eq!(
            service
                .exposure
                .as_ref()
                .map(|exposure| exposure.backend_port),
            Some(3000)
        );
        assert!(service.ports.is_empty());
    }

    #[test]
    fn planner_rejects_invalid_exposure_port() {
        let project = ComposeProject::from_yaml_str(
            r#"
services:
  web:
    image: nginx:latest
    ports:
      - "8080:80"
    labels:
      com.commandant.expose: "true"
      com.commandant.port: "not-a-port"
"#,
        )
        .expect("parse compose fixture");

        let error = planner::build_plan_with_proxy(&project, &[], &proxy::ProxyConfig::default())
            .expect_err("expected invalid exposure port");

        assert!(matches!(error, crate::Error::InvalidExposurePort { .. }));
    }

    fn temp_dir(prefix: &str) -> std::path::PathBuf {
        temp_dir_in(&std::env::temp_dir(), prefix)
    }

    fn temp_dir_in(root: &std::path::Path, prefix: &str) -> std::path::PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = root.join(format!("{prefix}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
