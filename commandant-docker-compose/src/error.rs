use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("failed to read compose file `{path}`: {source}")]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to parse compose file: {0}")]
    Parse(#[from] anyhow::Error),

    #[error("failed to serialize compose file: {0}")]
    Serialize(#[from] serde_yaml::Error),

    #[error("compose service `{service}` was not found")]
    UnknownService { service: String },

    #[error("compose network `{network}` was not found")]
    UnknownNetwork { network: String },

    #[error("compose volume `{volume}` was not found")]
    UnknownVolume { volume: String },

    #[error("compose file contains a dependency cycle involving `{service}`")]
    DependencyCycle { service: String },

    #[error("unsupported compose feature in service `{service}`: {feature}")]
    UnsupportedServiceFeature {
        service: String,
        feature: &'static str,
    },

    #[error("unsupported compose feature in network `{network}`: {feature}")]
    UnsupportedNetworkFeature {
        network: String,
        feature: &'static str,
    },

    #[error("invalid port mapping in service `{service}`: {value}")]
    InvalidPortMapping { service: String, value: String },

    #[error("invalid volume mapping in service `{service}`: {value}")]
    InvalidVolumeMapping { service: String, value: String },

    #[error("service `{service}` cannot be executed because it does not define an `image`")]
    MissingImage { service: String },

    #[error("invalid tag for service `{service}`: {tag}")]
    InvalidTag { service: String, tag: String },

    #[error("duplicate tag `{tag}` for services `{first}` and `{second}`")]
    DuplicateTag {
        tag: String,
        first: String,
        second: String,
    },

    #[error("service `{service}` is exposed but does not define a usable backend port")]
    MissingExposurePort { service: String },

    #[error("invalid exposure port for service `{service}`: {value}")]
    InvalidExposurePort { service: String, value: String },

    #[error("Docker runtime error: {0}")]
    Docker(#[from] lmrc_docker::DockerError),
}

pub type Result<T> = std::result::Result<T, Error>;
