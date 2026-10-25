//! OpenRPC 1.4 plugin documents and Chelix extensions.

use std::collections::BTreeMap;

use semver::Version;
use serde_json::{Map, Value};

use crate::address::{parse_address, valid_name};

#[derive(Debug, Clone)]
pub struct PluginDocument {
    pub raw: Value,
    pub name: String,
    pub api: String,
    pub version: Version,
    pub protocol: u64,
    pub description: String,
    pub requires: BTreeMap<String, String>,
    pub resources: Vec<ResourceDecl>,
    pub subscribes: Vec<String>,
    pub provides: Vec<ProvideDecl>,
    pub points: Vec<PointDecl>,
    pub events: Vec<EventDecl>,
    pub methods: Vec<MethodDecl>,
}

#[derive(Debug, Clone)]
pub struct ResourceDecl {
    pub id: String,
    pub kind: ResourceKind,
    pub access: Access,
    pub create: bool,
    pub file_mode: Option<u32>,
    pub from: Option<ResourceFrom>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceKind {
    Dir,
    File,
    Net { host: String, port: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
}

#[derive(Debug, Clone)]
pub struct ResourceFrom {
    pub root: String,
    pub path: String,
}

#[derive(Debug, Clone)]
pub struct ProvideDecl {
    pub point: String,
    pub order: i32,
    pub requires: Vec<String>,
    pub meta: Value,
}

#[derive(Debug, Clone)]
pub struct PointDecl {
    pub name: String,
    pub cardinality: Cardinality,
    pub params: Value,
    pub result: Value,
    pub meta_schema: Value,
    pub param_names: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cardinality {
    One,
    Many,
}

#[derive(Debug, Clone)]
pub struct EventDecl {
    pub name: String,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct MethodDecl {
    pub name: String,
    pub address: String,
    pub params: Value,
    pub result: Value,
    pub errors: Vec<i64>,
    pub examples: Vec<Value>,
    pub required: Vec<String>,
}

#[derive(Debug)]
pub struct DocumentError(pub String);

impl std::fmt::Display for DocumentError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for DocumentError {}

pub fn parse_document(raw: &Value) -> Result<PluginDocument, DocumentError> {
    let object = raw
        .as_object()
        .ok_or_else(|| DocumentError("document is not an object".into()))?;
    if object.get("openrpc").and_then(Value::as_str) != Some("1.4.0") {
        return Err(DocumentError("openrpc must be 1.4.0".into()));
    }
    let info = object
        .get("info")
        .and_then(Value::as_object)
        .ok_or_else(|| DocumentError("info is required".into()))?;
    let api = required_string(info, "info.title")?;
    if !valid_name(&api) {
        return Err(DocumentError(format!("api name `{api}` is invalid")));
    }
    let version = Version::parse(required_string(info, "info.version")?.as_str())
        .map_err(|error| DocumentError(format!("info.version: {error}")))?;
    let description = info
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    let plugin = object
        .get("x-chelix-plugin")
        .and_then(Value::as_object)
        .ok_or_else(|| DocumentError("x-chelix-plugin is required".into()))?;
    let name = required_string(plugin, "x-chelix-plugin.name")?;
    if !valid_name(&name) {
        return Err(DocumentError(format!("plugin name `{name}` is invalid")));
    }
    let protocol = plugin
        .get("protocol")
        .and_then(Value::as_u64)
        .ok_or_else(|| DocumentError("x-chelix-plugin.protocol is required".into()))?;
    let requires = read_requires(plugin.get("requires"))?;
    let resources = read_resources(plugin.get("resources"))?;
    let subscribes = read_string_list(plugin.get("subscribes"), "subscribes")?;
    let provides = read_provides(plugin.get("provides"))?;
    let points = read_points(object.get("x-chelix-points"))?;
    let events = read_events(object.get("x-chelix-events"))?;
    let methods = read_methods(object.get("methods"), &api)?;
    Ok(PluginDocument {
        raw: raw.clone(),
        name,
        api,
        version,
        protocol,
        description,
        requires,
        resources,
        subscribes,
        provides,
        points,
        events,
        methods,
    })
}

fn required_string(object: &Map<String, Value>, field: &str) -> Result<String, DocumentError> {
    object
        .get(field.rsplit('.').next().unwrap_or(field))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| DocumentError(format!("{field} is required")))
}

fn read_requires(value: Option<&Value>) -> Result<BTreeMap<String, String>, DocumentError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| DocumentError("requires must be an object".into()))?;
    let mut out = BTreeMap::new();
    for (api, range) in object {
        if !valid_name(api) {
            return Err(DocumentError(format!("requires key `{api}` is invalid")));
        }
        let range = range
            .as_str()
            .ok_or_else(|| DocumentError(format!("requires.{api} must be a string")))?;
        out.insert(api.clone(), range.to_owned());
    }
    Ok(out)
}

fn read_string_list(value: Option<&Value>, field: &str) -> Result<Vec<String>, DocumentError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| DocumentError(format!("{field} must be an array")))?;
    array
        .iter()
        .map(|item| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| DocumentError(format!("{field} entries must be strings")))
        })
        .collect()
}

fn read_resources(value: Option<&Value>) -> Result<Vec<ResourceDecl>, DocumentError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| DocumentError("resources must be an array".into()))?;
    array.iter().map(read_resource).collect()
}

fn read_resource(value: &Value) -> Result<ResourceDecl, DocumentError> {
    let object = value
        .as_object()
        .ok_or_else(|| DocumentError("resource must be an object".into()))?;
    let id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| DocumentError("resource.id is required".into()))?
        .to_owned();
    let kind = match object.get("kind") {
        Some(Value::String(kind)) if kind == "dir" => ResourceKind::Dir,
        Some(Value::String(kind)) if kind == "file" => ResourceKind::File,
        Some(Value::Object(kind)) => {
            let net = kind
                .get("net")
                .and_then(Value::as_str)
                .ok_or_else(|| DocumentError("resource.kind.net is required".into()))?;
            let (host, port) = net.rsplit_once(':').ok_or_else(|| {
                DocumentError(format!("resource.kind.net `{net}` is not host:port"))
            })?;
            let port = port
                .parse::<u16>()
                .map_err(|error| DocumentError(format!("resource.kind.net port: {error}")))?;
            if host.is_empty() {
                return Err(DocumentError("resource.kind.net host is empty".into()));
            }
            ResourceKind::Net {
                host: host.to_owned(),
                port,
            }
        },
        _ => return Err(DocumentError("resource.kind is invalid".into())),
    };
    let access = match object.get("access").and_then(Value::as_str) {
        Some("read") => Access::Read,
        Some("read_write") => Access::ReadWrite,
        _ => return Err(DocumentError(format!("resource {id} access is invalid"))),
    };
    let create = object.get("create").and_then(Value::as_bool).unwrap_or(false);
    let file_mode = match object.get("file_mode").and_then(Value::as_str) {
        Some(mode) => Some(
            u32::from_str_radix(mode, 8)
                .map_err(|error| DocumentError(format!("resource {id} file_mode: {error}")))?,
        ),
        None => None,
    };
    if access == Access::ReadWrite && file_mode.is_none() && !matches!(kind, ResourceKind::Net { .. })
    {
        return Err(DocumentError(format!(
            "resource {id} requires file_mode"
        )));
    }
    let from = match object.get("from") {
        None => None,
        Some(value) => {
            let from = value
                .as_object()
                .ok_or_else(|| DocumentError(format!("resource {id} from is invalid")))?;
            Some(ResourceFrom {
                root: from
                    .get("root")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DocumentError(format!("resource {id} from.root is required")))?
                    .to_owned(),
                path: from
                    .get("path")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DocumentError(format!("resource {id} from.path is required")))?
                    .to_owned(),
            })
        },
    };
    Ok(ResourceDecl {
        id,
        kind,
        access,
        create,
        file_mode,
        from,
    })
}

fn read_provides(value: Option<&Value>) -> Result<Vec<ProvideDecl>, DocumentError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| DocumentError("provides must be an array".into()))?;
    array
        .iter()
        .map(|item| {
            let object = item
                .as_object()
                .ok_or_else(|| DocumentError("provides entry must be an object".into()))?;
            let point = object
                .get("point")
                .and_then(Value::as_str)
                .ok_or_else(|| DocumentError("provides.point is required".into()))?;
            parse_address(point)
                .map_err(|error| DocumentError(format!("provides.point: {error}")))?;
            let order = object.get("order").and_then(Value::as_i64).unwrap_or(0) as i32;
            let requires = read_string_list(object.get("requires"), "provides.requires")?;
            let meta = object.get("meta").cloned().unwrap_or(Value::Object(Map::new()));
            Ok(ProvideDecl {
                point: point.to_owned(),
                order,
                requires,
                meta,
            })
        })
        .collect()
}

fn read_points(value: Option<&Value>) -> Result<Vec<PointDecl>, DocumentError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| DocumentError("x-chelix-points must be an array".into()))?;
    array
        .iter()
        .map(|item| {
            let object = item
                .as_object()
                .ok_or_else(|| DocumentError("point must be an object".into()))?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| DocumentError("point.name is required".into()))?
                .to_owned();
            if !valid_name(&name) {
                return Err(DocumentError(format!("point `{name}` is invalid")));
            }
            let cardinality = match object.get("cardinality").and_then(Value::as_str) {
                Some("one") => Cardinality::One,
                Some("many") => Cardinality::Many,
                _ => return Err(DocumentError(format!("point {name} cardinality is invalid"))),
            };
            let params = object.get("params").cloned().unwrap_or(Value::Object(Map::new()));
            let result = object.get("result").cloned().unwrap_or(Value::Object(Map::new()));
            let meta_schema = object
                .get("meta")
                .cloned()
                .unwrap_or(Value::Object(Map::new()));
            let required = schema_required(&params);
            let param_names = schema_properties(&params);
            Ok(PointDecl {
                name,
                cardinality,
                params,
                result,
                meta_schema,
                param_names: required.into_iter().chain(param_names).collect(),
            })
        })
        .collect()
}

fn read_events(value: Option<&Value>) -> Result<Vec<EventDecl>, DocumentError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| DocumentError("x-chelix-events must be an array".into()))?;
    array
        .iter()
        .map(|item| {
            let object = item
                .as_object()
                .ok_or_else(|| DocumentError("event must be an object".into()))?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| DocumentError("event.name is required".into()))?
                .to_owned();
            if !valid_name(&name) {
                return Err(DocumentError(format!("event `{name}` is invalid")));
            }
            Ok(EventDecl {
                name,
                payload: object
                    .get("payload")
                    .cloned()
                    .unwrap_or(Value::Object(Map::new())),
            })
        })
        .collect()
}

fn read_methods(value: Option<&Value>, api: &str) -> Result<Vec<MethodDecl>, DocumentError> {
    let array = value
        .and_then(Value::as_array)
        .ok_or_else(|| DocumentError("methods must be an array".into()))?;
    array
        .iter()
        .map(|item| read_method(item, api))
        .collect()
}

fn read_method(value: &Value, api: &str) -> Result<MethodDecl, DocumentError> {
    let object = value
        .as_object()
        .ok_or_else(|| DocumentError("method must be an object".into()))?;
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| DocumentError("method.name is required".into()))?
        .to_owned();
    let address = format!("{api}.{name}");
    parse_address(&address).map_err(|error| DocumentError(error.to_string()))?;
    if object.get("paramStructure").and_then(Value::as_str) != Some("by-name") {
        return Err(DocumentError(format!("{address} paramStructure must be by-name")));
    }
    let params = descriptors_to_schema(object.get("params"))?;
    let result = object
        .get("result")
        .and_then(|result| result.get("schema"))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let errors = object
        .get("errors")
        .and_then(Value::as_array)
        .map(|errors| {
            errors
                .iter()
                .filter_map(|error| error.get("code").and_then(Value::as_i64))
                .collect()
        })
        .unwrap_or_default();
    let examples = object
        .get("examples")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if examples.is_empty() {
        return Err(DocumentError(format!("{address} requires an example")));
    }
    Ok(MethodDecl {
        required: schema_required(&params),
        name,
        address,
        params,
        result,
        errors,
        examples,
    })
}

fn descriptors_to_schema(value: Option<&Value>) -> Result<Value, DocumentError> {
    let Some(Value::Array(params)) = value else {
        return Ok(Value::Object(Map::from_iter([(
            "type".to_owned(),
            Value::from("object"),
        )])));
    };
    let mut properties = Map::new();
    let mut required = Vec::new();
    for param in params {
        let object = param
            .as_object()
            .ok_or_else(|| DocumentError("method param must be an object".into()))?;
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| DocumentError("method param name is required".into()))?;
        let schema = object
            .get("schema")
            .cloned()
            .ok_or_else(|| DocumentError(format!("method param {name} schema is required")))?;
        if object.get("required").and_then(Value::as_bool).unwrap_or(false) {
            required.push(Value::from(name));
        }
        properties.insert(name.to_owned(), schema);
    }
    let mut schema = Map::new();
    schema.insert("type".into(), Value::from("object"));
    schema.insert("properties".into(), Value::Object(properties));
    schema.insert("required".into(), Value::Array(required));
    schema.insert("additionalProperties".into(), Value::from(false));
    Ok(Value::Object(schema))
}

fn schema_required(schema: &Value) -> Vec<String> {
    schema
        .get("required")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn schema_properties(schema: &Value) -> Vec<String> {
    schema
        .get("properties")
        .and_then(Value::as_object)
        .map(|properties| properties.keys().cloned().collect())
        .unwrap_or_default()
}

pub fn bus_points() -> Vec<PointDecl> {
    vec![
        PointDecl {
            name: "roots".into(),
            cardinality: Cardinality::Many,
            params: Value::Object(Map::new()),
            result: serde_json::json!({
                "type": "object",
                "additionalProperties": { "type": "string" }
            }),
            meta_schema: Value::Object(Map::new()),
            param_names: Vec::new(),
        },
        PointDecl {
            name: "activation".into(),
            cardinality: Cardinality::One,
            params: Value::Object(Map::new()),
            result: serde_json::json!({
                "type": "object",
                "properties": {
                    "disabled": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                },
                "required": ["disabled"],
                "additionalProperties": false
            }),
            meta_schema: Value::Object(Map::new()),
            param_names: Vec::new(),
        },
    ]
}
