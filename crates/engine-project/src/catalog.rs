//! Declarative authoring schemas and typed native ECS adapters.
use crate::{Error, Result};
use bevy_ecs::prelude::*;
use engine_core::EngineApp;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CustomValue {
    pub version: u32,
    pub data: Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ComponentDescriptor {
    pub name: String,
    pub version: u32,
    pub schema: Value,
    pub defaults: Value,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub entity_references: Vec<String>,
    #[serde(default)]
    pub data_only: bool,
}
#[derive(Clone, Debug, Default)]
pub struct ComponentCatalog(pub BTreeMap<String, ComponentDescriptor>);
fn invalid(message: impl Into<String>) -> Error {
    Error::new("component.schema.invalid", message)
}
impl ComponentCatalog {
    pub fn new(descriptors: Vec<ComponentDescriptor>) -> Result<Self> {
        if descriptors.len() > 256 {
            return Err(invalid("At most 256 custom component types"));
        }
        if descriptors.iter().any(|d| d.name.starts_with("engine.")) {
            return Err(invalid(
                "The engine namespace is reserved for built-in components",
            ));
        }
        let mut result = Self::default();
        for d in crate::simulation::component_descriptors()
            .into_iter()
            .chain(crate::ui_tree::descriptors())
            .chain([
                crate::accessibility::descriptor(),
                crate::captions::descriptor(),
                crate::video::descriptor(),
                crate::sequencer::descriptor(),
                crate::world_text::descriptor(),
                crate::xr::descriptor(),
            ])
            .chain(crate::acoustics::descriptors())
            .chain(crate::ai::descriptors())
            .chain(crate::networking::descriptors())
            .chain(descriptors)
        {
            if d.name.len() > 128
                || !d.name.contains('.')
                || d.name
                    .chars()
                    .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')))
                || d.version == 0
            {
                return Err(invalid(
                    "Custom names must be namespaced identifiers and versions positive",
                ));
            }
            check_schema(&d.schema, 0)?;
            validate_value(&d.schema, &d.defaults, 0)?;
            if d.entity_references.len() > 64
                || d.entity_references.iter().any(|p| !p.starts_with('/'))
            {
                return Err(invalid("Entity references must be JSON pointers"));
            }
            if result.0.insert(d.name.clone(), d).is_some() {
                return Err(invalid("Duplicate custom component name"));
            }
        }
        Ok(result)
    }
    pub fn validate(&self, name: &str, value: &CustomValue) -> Result<()> {
        let descriptor = self
            .0
            .get(name)
            .ok_or_else(|| invalid(format!("Unknown component {name}")))?;
        if value.version != descriptor.version {
            return Err(invalid(format!("Unsupported {name} version")));
        }
        if serde_json::to_vec(&value.data)?.len() > 65536 {
            return Err(invalid("Component exceeds 64KiB"));
        }
        validate_value(&descriptor.schema, &value.data, 0)?;
        crate::ui_tree::validate_component(name, &value.data)?;
        crate::accessibility::validate_component(name, &value.data)?;
        crate::captions::validate_component(name, &value.data)?;
        crate::sequencer::validate_component(name, &value.data)?;
        if name == crate::video::VIDEO_PLAYER_COMPONENT {
            serde_json::from_value::<crate::video::VideoPlayerComponent>(value.data.clone())?
                .validate()?;
        }
        if name == crate::world_text::WORLD_TEXT_COMPONENT {
            serde_json::from_value::<crate::world_text::WorldText>(value.data.clone())?
                .validate()?;
        }
        if name == crate::xr::XR_SETTINGS_COMPONENT {
            serde_json::from_value::<crate::xr::XrSettings>(value.data.clone())?.validate()?;
        }
        crate::acoustics::validate_component(name, &value.data)?;
        crate::networking::validate_component(name, &value.data)?;
        Ok(())
    }
    pub fn validate_entity(&self, scene: &crate::Scene, entity: &crate::SceneEntity) -> Result<()> {
        let serialized = serde_json::to_value(&entity.components)?;
        for (name, value) in &entity.components.custom {
            self.validate(name, value)?;
            let descriptor = &self.0[name];
            for required in &descriptor.requires {
                if !entity.components.custom.contains_key(required)
                    && serialized.get(required).is_none()
                {
                    return Err(invalid(format!("{name} requires {required}")));
                }
            }
            for pointer in &descriptor.entity_references {
                if let Some(reference) = value.data.pointer(pointer).filter(|v| !v.is_null()) {
                    let id = reference
                        .as_str()
                        .ok_or_else(|| invalid("Entity reference must be a string"))?;
                    if !scene.entities.iter().any(|e| e.id == id) {
                        return Err(invalid(format!("Unknown entity reference {id}")));
                    }
                }
            }
        }
        Ok(())
    }
}
fn check_schema(schema: &Value, depth: usize) -> Result<()> {
    if depth > 16 {
        return Err(invalid("Schema nesting exceeds 16"));
    }
    let object = schema
        .as_object()
        .ok_or_else(|| invalid("Schema must be an object"))?;
    for key in object.keys() {
        if ![
            "type",
            "properties",
            "required",
            "additionalProperties",
            "items",
            "minItems",
            "maxItems",
            "minimum",
            "maximum",
            "minLength",
            "maxLength",
            "enum",
            "description",
            "default",
        ]
        .contains(&key.as_str())
        {
            return Err(invalid(format!("Unsupported schema keyword {key}")));
        }
    }
    let kind = schema
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("Schema requires type"))?;
    if ![
        "object", "array", "string", "number", "integer", "boolean", "null",
    ]
    .contains(&kind)
    {
        return Err(invalid("Unsupported schema type"));
    }
    if let Some(properties) = schema.get("properties") {
        for value in properties
            .as_object()
            .ok_or_else(|| invalid("properties must be an object"))?
            .values()
        {
            check_schema(value, depth + 1)?;
        }
    }
    if let Some(items) = schema.get("items") {
        check_schema(items, depth + 1)?;
    }
    if let Some(required) = schema.get("required") {
        let values = required
            .as_array()
            .ok_or_else(|| invalid("required must be an array"))?;
        if values.iter().any(|v| v.as_str().is_none()) {
            return Err(invalid("required entries must be strings"));
        }
    }
    if schema
        .get("additionalProperties")
        .is_some_and(|v| !v.is_boolean())
    {
        return Err(invalid("additionalProperties must be boolean"));
    }
    for key in ["minItems", "maxItems", "minLength", "maxLength"] {
        if schema.get(key).is_some_and(|v| v.as_u64().is_none()) {
            return Err(invalid(format!("{key} must be nonnegative integer")));
        }
    }
    for key in ["minimum", "maximum"] {
        if schema.get(key).is_some_and(|v| !v.is_number()) {
            return Err(invalid(format!("{key} must be number")));
        }
    }
    if schema
        .get("enum")
        .is_some_and(|v| v.as_array().is_none_or(Vec::is_empty))
    {
        return Err(invalid("enum must be nonempty array"));
    }
    Ok(())
}
fn validate_value(schema: &Value, value: &Value, depth: usize) -> Result<()> {
    if depth > 32 {
        return Err(invalid("Value nesting exceeds 32"));
    }
    let valid = match schema["type"].as_str().unwrap_or("") {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => false,
    };
    if !valid {
        return Err(invalid("Component field has incorrect type"));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array)
        && !values.contains(value)
    {
        return Err(invalid("Value not in enum"));
    }
    if let Some(object) = value.as_object() {
        if let Some(required) = schema.get("required").and_then(Value::as_array) {
            for key in required {
                if !object.contains_key(key.as_str().unwrap_or("")) {
                    return Err(invalid(format!("Missing field {key}")));
                }
            }
        }
        for (key, value) in object {
            if let Some(child) = schema.get("properties").and_then(|v| v.get(key)) {
                validate_value(child, value, depth + 1)?;
            } else if schema.get("additionalProperties").and_then(Value::as_bool) == Some(false) {
                return Err(invalid(format!("Unknown field {key}")));
            }
        }
    }
    if let Some(values) = value.as_array() {
        bound(schema, "minItems", "maxItems", values.len() as f64)?;
        if let Some(items) = schema.get("items") {
            for value in values {
                validate_value(items, value, depth + 1)?;
            }
        }
    }
    if let Some(text) = value.as_str() {
        bound(
            schema,
            "minLength",
            "maxLength",
            text.chars().count() as f64,
        )?;
    }
    if let Some(number) = value.as_f64() {
        bound(schema, "minimum", "maximum", number)?;
    }
    Ok(())
}
fn bound(schema: &Value, min: &str, max: &str, value: f64) -> Result<()> {
    if schema
        .get(min)
        .and_then(Value::as_f64)
        .is_some_and(|n| value < n)
        || schema
            .get(max)
            .and_then(Value::as_f64)
            .is_some_and(|n| value > n)
    {
        return Err(invalid("Component field outside bounds"));
    }
    Ok(())
}

#[derive(Component, Clone, Debug, Default)]
pub struct DynamicComponents(pub BTreeMap<String, CustomValue>);
#[derive(Clone, Copy)]
pub(crate) struct ComponentAdapter {
    pub version: u32,
    type_id: std::any::TypeId,
    insert: fn(&mut World, Entity, &Value) -> Result<()>,
    encode: fn(&World, Entity) -> Result<Option<Value>>,
}
#[derive(Resource, Default)]
pub(crate) struct ComponentRegistry(pub BTreeMap<String, ComponentAdapter>);
pub fn register_component<T>(app: &mut EngineApp, name: &str, version: u32) -> Result<()>
where
    T: Component + Serialize + DeserializeOwned,
{
    let project = app
        .world()
        .get_resource::<crate::runtime::ProjectResource>()
        .ok_or_else(|| invalid("Runtime project unavailable"))?;
    let d = project
        .0
        .component_catalog
        .0
        .get(name)
        .ok_or_else(|| invalid(format!("Undeclared component {name}")))?;
    if d.version != version || d.data_only {
        return Err(invalid(
            "Adapter version or storage mode disagrees with descriptor",
        ));
    }
    if !app.world().contains_resource::<ComponentRegistry>() {
        app.world_mut()
            .insert_resource(ComponentRegistry::default());
    }
    let mut registry = app.world_mut().resource_mut::<ComponentRegistry>();
    if registry.0.contains_key(name)
        || registry
            .0
            .values()
            .any(|a| a.type_id == std::any::TypeId::of::<T>())
    {
        return Err(invalid("Component adapter already registered"));
    }
    registry.0.insert(
        name.into(),
        ComponentAdapter {
            version,
            type_id: std::any::TypeId::of::<T>(),
            insert: |world, entity, value| {
                let value: T = serde_json::from_value(value.clone())?;
                world.entity_mut(entity).insert(value);
                Ok(())
            },
            encode: |world, entity| {
                world
                    .get::<T>(entity)
                    .map(serde_json::to_value)
                    .transpose()
                    .map_err(Into::into)
            },
        },
    );
    Ok(())
}
pub(crate) fn hydrate(
    world: &mut World,
    scene: &crate::Scene,
    catalog: &ComponentCatalog,
) -> Result<()> {
    for authored in &scene.entities {
        let entity = crate::services::find_entity(world, &authored.id)
            .ok_or_else(|| invalid("Missing authored entity"))?;
        for (name, value) in &authored.components.custom {
            catalog.validate(name, value)?;
            if catalog.0[name].data_only {
                continue;
            }
            let adapter = world
                .get_resource::<ComponentRegistry>()
                .and_then(|r| r.0.get(name))
                .copied()
                .ok_or_else(|| invalid(format!("No native adapter for {name}")))?;
            if adapter.version != value.version {
                return Err(invalid("Native adapter version mismatch"));
            }
            (adapter.insert)(world, entity, &value.data)?;
        }
    }
    Ok(())
}
pub(crate) fn snapshot(world: &World, entity: Entity) -> Result<BTreeMap<String, CustomValue>> {
    let mut values = world
        .get::<DynamicComponents>(entity)
        .map(|d| d.0.clone())
        .unwrap_or_default();
    if let Some(registry) = world.get_resource::<ComponentRegistry>() {
        for (name, adapter) in &registry.0 {
            if let Some(data) = (adapter.encode)(world, entity)? {
                values.insert(
                    name.clone(),
                    CustomValue {
                        version: adapter.version,
                        data,
                    },
                );
            } else {
                values.remove(name);
            }
        }
    }
    if let Some(project) = world.get_resource::<crate::runtime::ProjectResource>() {
        for (name, value) in &values {
            project.0.component_catalog.validate(name, value)?;
        }
    }
    Ok(values)
}
