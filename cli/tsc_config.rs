// Copyright 2018-2020 the Deno authors. All rights reserved. MIT license.

use deno_core::error::AnyError;
use deno_core::serde_json;
use deno_core::serde_json::Value;
use jsonc_parser::JsonValue;
use serde::Deserialize;
use serde::Serialize;
use serde::Serializer;
use std::collections::HashMap;
use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

/// The transpile options that are significant out of a user provided tsconfig
/// file, that we want to deserialize out of the final config for a transpile.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranspileConfigOptions {
  pub check_js: bool,
  pub emit_decorator_metadata: bool,
  pub jsx: String,
  pub jsx_factory: String,
  pub jsx_fragment_factory: String,
}

/// A structure that represents a set of options that were ignored and the
/// path those options came from.
#[derive(Debug, Clone, PartialEq)]
pub struct IgnoredCompilerOptions {
  pub items: Vec<String>,
  pub path: PathBuf,
}

impl fmt::Display for IgnoredCompilerOptions {
  fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
    let mut codes = self.items.clone();
    codes.sort();

    write!(f, "Unsupported compiler options in \"{}\".\n  The following options were ignored:\n    {}", self.path.to_string_lossy(), codes.join(", "))
  }
}

/// A static slice of all the compiler options that should be ignored that
/// either have no effect on the compilation or would cause the emit to not work
/// in Deno.
const IGNORED_COMPILER_OPTIONS: [&str; 61] = [
  "allowSyntheticDefaultImports",
  "allowUmdGlobalAccess",
  "assumeChangesOnlyAffectDirectDependencies",
  "baseUrl",
  "build",
  "composite",
  "declaration",
  "declarationDir",
  "declarationMap",
  "diagnostics",
  "downlevelIteration",
  "emitBOM",
  "emitDeclarationOnly",
  "esModuleInterop",
  "extendedDiagnostics",
  "forceConsistentCasingInFileNames",
  "generateCpuProfile",
  "help",
  "importHelpers",
  "incremental",
  "inlineSourceMap",
  "inlineSources",
  "init",
  "listEmittedFiles",
  "listFiles",
  "mapRoot",
  "maxNodeModuleJsDepth",
  "module",
  "moduleResolution",
  "newLine",
  "noEmit",
  "noEmitHelpers",
  "noEmitOnError",
  "noLib",
  "noResolve",
  "out",
  "outDir",
  "outFile",
  "paths",
  "preserveConstEnums",
  "preserveSymlinks",
  "preserveWatchOutput",
  "pretty",
  "reactNamespace",
  "resolveJsonModule",
  "rootDir",
  "rootDirs",
  "showConfig",
  "skipDefaultLibCheck",
  "skipLibCheck",
  "sourceMap",
  "sourceRoot",
  "stripInternal",
  "target",
  "traceResolution",
  "tsBuildInfoFile",
  "types",
  "typeRoots",
  "useDefineForClassFields",
  "version",
  "watch",
];

/// A function that works like JavaScript's `Object.assign()`.
pub fn json_merge(a: &mut Value, b: &Value) {
  match (a, b) {
    (&mut Value::Object(ref mut a), &Value::Object(ref b)) => {
      for (k, v) in b {
        json_merge(a.entry(k.clone()).or_insert(Value::Null), v);
      }
    }
    (a, b) => {
      *a = b.clone();
    }
  }
}

/// Convert a jsonc libraries `JsonValue` to a serde `Value`.
fn jsonc_to_serde(j: JsonValue) -> Value {
  match j {
    JsonValue::Array(arr) => {
      let vec = arr.into_iter().map(jsonc_to_serde).collect();
      Value::Array(vec)
    }
    JsonValue::Boolean(bool) => Value::Bool(bool),
    JsonValue::Null => Value::Null,
    JsonValue::Number(num) => {
      let number =
        serde_json::Number::from_str(&num).expect("could not parse number");
      Value::Number(number)
    }
    JsonValue::Object(obj) => {
      let mut map = serde_json::map::Map::new();
      for (key, json_value) in obj.into_iter() {
        map.insert(key, jsonc_to_serde(json_value));
      }
      Value::Object(map)
    }
    JsonValue::String(str) => Value::String(str),
  }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TSConfigJson {
  compiler_options: Option<HashMap<String, Value>>,
  exclude: Option<Vec<String>>,
  extends: Option<String>,
  files: Option<Vec<String>>,
  include: Option<Vec<String>>,
  references: Option<Value>,
  type_acquisition: Option<Value>,
}

pub fn parse_raw_config(config_text: &str) -> Result<Value, AnyError> {
  assert!(!config_text.is_empty());
  let jsonc = jsonc_parser::parse_to_value(config_text)?.unwrap();
  Ok(jsonc_to_serde(jsonc))
}

/// Take a string of JSONC, parse it and return a serde `Value` of the text.
/// The result also contains any options that were ignored.
pub fn parse_config(
  config_text: &str,
  path: &Path,
) -> Result<(Value, Option<IgnoredCompilerOptions>), AnyError> {
  assert!(!config_text.is_empty());
  let jsonc = jsonc_parser::parse_to_value(config_text)?.unwrap();
  let config: TSConfigJson = serde_json::from_value(jsonc_to_serde(jsonc))?;
  let mut compiler_options: HashMap<String, Value> = HashMap::new();
  let mut items: Vec<String> = Vec::new();

  if let Some(in_compiler_options) = config.compiler_options {
    for (key, value) in in_compiler_options.iter() {
      if IGNORED_COMPILER_OPTIONS.contains(&key.as_str()) {
        items.push(key.to_owned());
      } else {
        compiler_options.insert(key.to_owned(), value.to_owned());
      }
    }
  }
  let options_value = serde_json::to_value(compiler_options)?;
  let ignored_options = if !items.is_empty() {
    Some(IgnoredCompilerOptions {
      items,
      path: path.to_path_buf(),
    })
  } else {
    None
  };

  Ok((options_value, ignored_options))
}

/// A structure for managing the configuration of TypeScript
#[derive(Debug, Clone)]
pub struct TsConfig(Value);

impl TsConfig {
  /// Create a new `TsConfig` with the base being the `value` supplied.
  pub fn new(value: Value) -> Self {
    TsConfig(value)
  }

  /// Take an optional string representing a user provided TypeScript config file
  /// which was passed in via the `--config` compiler option and merge it with
  /// the configuration.  Returning the result which optionally contains any
  /// compiler options that were ignored.
  ///
  /// When there are options ignored out of the file, a warning will be written
  /// to stderr regarding the options that were ignored.
  pub fn merge_user_config(
    &mut self,
    maybe_path: Option<String>,
  ) -> Result<Option<IgnoredCompilerOptions>, AnyError> {
    if let Some(path) = maybe_path {
      let cwd = std::env::current_dir()?;
      let config_file = cwd.join(path);
      let config_path = config_file.canonicalize().map_err(|_| {
        std::io::Error::new(
          std::io::ErrorKind::InvalidInput,
          format!(
            "Could not find the config file: {}",
            config_file.to_string_lossy()
          ),
        )
      })?;
      let config_text = std::fs::read_to_string(config_path.clone())?;
      let (value, maybe_ignored_options) =
        parse_config(&config_text, &config_path)?;
      json_merge(&mut self.0, &value);

      Ok(maybe_ignored_options)
    } else {
      Ok(None)
    }
  }

  /// Return the current configuration as a `TranspileConfigOptions` structure.
  pub fn as_transpile_config(
    &self,
  ) -> Result<TranspileConfigOptions, AnyError> {
    let options: TranspileConfigOptions =
      serde_json::from_value(self.0.clone())?;
    Ok(options)
  }
}

impl Serialize for TsConfig {
  /// Serializes inner hash map which is ordered by the key
  fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    Serialize::serialize(&self.0, serializer)
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use deno_core::serde_json::json;

  #[test]
  fn test_json_merge() {
    let mut value_a = json!({
      "a": true,
      "b": "c"
    });
    let value_b = json!({
      "b": "d",
      "e": false,
    });
    json_merge(&mut value_a, &value_b);
    assert_eq!(
      value_a,
      json!({
        "a": true,
        "b": "d",
        "e": false,
      })
    );
  }

  #[test]
  fn test_parse_config() {
    let config_text = r#"{
      "compilerOptions": {
        "build": true,
        // comments are allowed
        "strict": true
      }
    }"#;
    let config_path = PathBuf::from("/deno/tsconfig.json");
    let (options_value, ignored) =
      parse_config(config_text, &config_path).expect("error parsing");
    assert!(options_value.is_object());
    let options = options_value.as_object().unwrap();
    assert!(options.contains_key("strict"));
    assert_eq!(options.len(), 1);
    assert_eq!(
      ignored,
      Some(IgnoredCompilerOptions {
        items: vec!["build".to_string()],
        path: config_path,
      }),
    );
  }

  #[test]
  fn test_parse_raw_config() {
    let invalid_config_text = r#"{
      "compilerOptions": {
        // comments are allowed
    }"#;
    let errbox = parse_raw_config(invalid_config_text).unwrap_err();
    assert!(errbox
      .to_string()
      .starts_with("Unterminated object on line 1"));
  }
}
