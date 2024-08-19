//! parsing google fonts config files

use std::path::Path;

use font_types::Tag;

use crate::error::BadConfig;

/// Google fonts config file ('config.yaml')
///
/// This is a standard file that describes the sources and steps for building a
/// font. See [googlefonts-project-template][template].
///
/// [template]: https://github.com/googlefonts/googlefonts-project-template/blob/main/sources/config.yaml
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
// there are a bunch of other fields here we may need to add in the future
#[non_exhaustive]
pub struct Config {
    pub sources: Vec<String>,
    pub family_name: Option<String>,
    #[serde(default)]
    pub build_variable: bool,
    #[serde(default)]
    pub axis_order: Vec<Tag>,
}

impl Config {
    /// Parse and return a config.yaml file for the provided font source
    pub fn load(config_path: &Path) -> Result<Self, BadConfig> {
        let contents = std::fs::read_to_string(config_path)?;
        serde_yaml::from_str(&contents).map_err(BadConfig::Yaml)
    }
}
