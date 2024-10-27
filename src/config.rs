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
    #[serde(default = "true_")]
    pub build_variable: bool,
    #[serde(default = "true_")]
    pub build_static: bool,
    #[serde(default = "true_")]
    pub build_ttf: bool,
    #[serde(default)]
    pub build_otf: bool,
    #[serde(default)]
    pub axis_order: Vec<Tag>,
    pub recipe_provider: Option<String>,
    #[serde(default)]
    pub glyph_data: Vec<String>,

    // build options
    #[serde(default = "true_")]
    pub flatten_components: bool,
    #[serde(default = "true_")]
    pub decompose_transformed_components: bool,
    #[serde(default = "true_")]
    pub reverse_outline_direction: bool,
    #[serde(default = "true_")]
    pub check_compatibility: bool,
    #[serde(default = "true_")]
    pub remove_outline_overlaps: bool,
    #[serde(default)]
    pub expand_features_to_instances: bool,

    #[serde(default = "true_")]
    pub build_small_cap: bool,
    #[serde(default = "true_")]
    pub split_italic: bool,
}

fn true_() -> bool {
    true
}

impl Config {
    /// Parse and return a config.yaml file for the provided font source
    pub fn load(config_path: &Path) -> Result<Self, BadConfig> {
        let contents = std::fs::read_to_string(config_path)?;
        serde_yaml::from_str(&contents).map_err(BadConfig::Yaml)
    }
}
