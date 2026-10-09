//! JSON ⇄ reflection for MCP tools.
//!
//! Component and material values are shown as plain JSON (structs → objects, vectors and
//! colors → arrays, enums → variant names, assets → `{id, path}`, entities → ids). Input is
//! converted by walking the target's reflected shape, so field names and enum variants are
//! checked and partial objects only touch the fields they name.

use dumb_asset::{primitives::BUILTIN_MODELS, AssetDatabase};
use dumb_core::{AssetId, Entity};
use dumb_reflect::{Reflect, ReflectRef, Value};
use serde_json::{json, Map, Value as Json};

/// An asset reference: `null` for none, otherwise `{"id", "path"}` (path or display name).
pub fn asset_json(id: AssetId, db: &AssetDatabase) -> Json {
    if id.is_none() {
        return Json::Null;
    }
    let path = db.entry(id).map(|e| e.path.clone()).unwrap_or_else(|| db.display_name(id));
    json!({ "id": id.0.to_string(), "path": path })
}

/// Accepts a UUID, a path under `Assets/` (with or without the `Assets/` prefix), a built-in
/// name ("Cube"), or an object with an `id` or `path` field.
pub fn resolve_asset(j: &Json, db: &AssetDatabase) -> Result<AssetId, String> {
    let s = match j {
        Json::Null => return Ok(AssetId::NONE),
        Json::String(s) => s.trim(),
        Json::Object(o) => {
            return match o.get("id").filter(|v| !v.is_null()).or_else(|| o.get("path")) {
                Some(v) => resolve_asset(v, db),
                None => Err("asset object needs an `id` or `path`".into()),
            }
        }
        _ => return Err(format!("expected an asset (UUID or path), got {j}")),
    };
    if s.is_empty() || s.eq_ignore_ascii_case("none") {
        return Ok(AssetId::NONE);
    }
    if let Ok(u) = uuid::Uuid::parse_str(s) {
        return Ok(AssetId(u));
    }
    let rel = s.trim_start_matches("Assets/").trim_start_matches("Assets\\").replace('\\', "/");
    if let Some(id) = db.id_for_path(&rel) {
        return Ok(id);
    }
    let bare = s.trim_end_matches(" (built-in)");
    if let Some((id, _)) = BUILTIN_MODELS.iter().find(|(_, n)| n.eq_ignore_ascii_case(bare)) {
        return Ok(*id);
    }
    Err(format!("no asset `{s}` (use a path under Assets/, a UUID, or a built-in name: {})", builtin_names()))
}

fn builtin_names() -> String {
    BUILTIN_MODELS.iter().map(|(_, n)| *n).collect::<Vec<_>>().join(", ")
}

pub fn entity_json(e: Entity) -> Json {
    if e.is_none() {
        Json::Null
    } else {
        json!(e.to_bits())
    }
}

fn floats(v: &[f32]) -> Json {
    Json::Array(v.iter().map(|f| json!(round(*f as f64))).collect())
}

/// Trim float noise (0.30000001 → 0.3) so values read cleanly.
fn round(f: f64) -> f64 {
    if !f.is_finite() {
        return 0.0;
    }
    (f * 1e6).round() / 1e6
}

/// Reflected value → JSON.
pub fn reflect_to_json(r: &dyn Reflect, db: &AssetDatabase) -> Json {
    match r.reflect_ref() {
        ReflectRef::Struct(s) => {
            let mut m = Map::new();
            for (i, f) in s.fields().iter().enumerate() {
                m.insert(f.name.to_string(), reflect_to_json(s.field(i), db));
            }
            Json::Object(m)
        }
        ReflectRef::Enum(e) => json!(e.variants()[e.variant_index()]),
        ReflectRef::List(l) => Json::Array((0..l.len()).map(|i| reflect_to_json(l.get(i), db)).collect()),
        ReflectRef::Bool(v) => json!(v),
        ReflectRef::I32(v) => json!(v),
        ReflectRef::U32(v) => json!(v),
        ReflectRef::I64(v) => json!(v),
        ReflectRef::U64(v) => json!(v),
        ReflectRef::F32(v) => json!(round(*v as f64)),
        ReflectRef::F64(v) => json!(round(*v)),
        ReflectRef::String(v) => json!(v),
        ReflectRef::Vec2(v) => floats(&v.to_array()),
        ReflectRef::Vec3(v) => floats(&v.to_array()),
        ReflectRef::Vec4(v) => floats(&v.to_array()),
        ReflectRef::Quat(v) => floats(&v.to_array()),
        ReflectRef::Color(v) => floats(&v.to_array()),
        ReflectRef::Asset(v) => asset_json(*v, db),
        ReflectRef::Entity(v) => entity_json(*v),
        ReflectRef::Opaque(s) => json!({ "opaque": s }),
    }
}

/// Untyped value tree → JSON (components whose type is not loaded).
pub fn value_to_json(v: &Value, db: &AssetDatabase) -> Json {
    match v {
        Value::None => Json::Null,
        Value::Bool(b) => json!(b),
        Value::Int(i) => json!(i),
        Value::Float(f) => json!(round(*f)),
        Value::String(s) => json!(s),
        Value::Vec(v) => floats(v),
        Value::Asset(a) => asset_json(*a, db),
        Value::Entity(e) => entity_json(Entity::from_bits(*e)),
        Value::Enum(s) => json!(s),
        Value::List(l) => Json::Array(l.iter().map(|x| value_to_json(x, db)).collect()),
        Value::Struct(f) => Json::Object(f.iter().map(|(n, x)| (n.clone(), value_to_json(x, db))).collect()),
    }
}

/// Best-effort JSON → value without a type to guide it (list items of an empty list).
fn untyped(j: &Json) -> Value {
    match j {
        Json::Null => Value::None,
        Json::Bool(b) => Value::Bool(*b),
        Json::Number(n) => n.as_i64().map_or_else(|| Value::Float(n.as_f64().unwrap_or(0.0)), Value::Int),
        Json::String(s) => Value::String(s.clone()),
        Json::Array(a) if !a.is_empty() && a.iter().all(Json::is_number) => Value::Vec(a.iter().map(|x| x.as_f64().unwrap_or(0.0) as f32).collect()),
        Json::Array(a) => Value::List(a.iter().map(untyped).collect()),
        Json::Object(o) => Value::Struct(o.iter().map(|(k, v)| (k.clone(), untyped(v))).collect()),
    }
}

fn num(j: &Json, path: &str) -> Result<f64, String> {
    match j {
        Json::Number(n) => n.as_f64().ok_or_else(|| format!("{path}: bad number")),
        Json::Bool(b) => Ok(*b as i32 as f64),
        _ => Err(format!("{path}: expected a number, got {j}")),
    }
}

/// `[x, y, z]`, or `{"x":..,"y":..}` (and `r g b a` for colors). Colors may omit alpha.
fn vector(j: &Json, n: usize, color: bool, path: &str) -> Result<Value, String> {
    let mut out: Vec<f32> = match j {
        Json::Array(a) => a.iter().map(|x| num(x, path).map(|f| f as f32)).collect::<Result<_, _>>()?,
        Json::Object(o) => {
            let keys: &[&str] = if color { &["r", "g", "b", "a"] } else { &["x", "y", "z", "w"] };
            keys[..n].iter().filter_map(|k| o.get(*k)).map(|x| num(x, path).map(|f| f as f32)).collect::<Result<_, _>>()?
        }
        _ => return Err(format!("{path}: expected an array of {n} numbers, got {j}")),
    };
    if color && n == 4 && out.len() == 3 {
        out.push(1.0);
    }
    if out.len() != n {
        return Err(format!("{path}: expected {n} numbers, got {}", out.len()));
    }
    Ok(Value::Vec(out))
}

/// JSON → value tree shaped like `r`, ready for [`dumb_reflect::apply`]. Objects may name only
/// some fields; unknown fields and enum variants are errors.
pub fn json_to_value(j: &Json, r: &dyn Reflect, db: &AssetDatabase, path: &str) -> Result<Value, String> {
    let at = |seg: &str| if path.is_empty() { seg.to_string() } else { format!("{path}.{seg}") };
    let here = if path.is_empty() { "value" } else { path };
    Ok(match r.reflect_ref() {
        ReflectRef::Struct(s) => {
            let Json::Object(o) = j else { return Err(format!("{here}: expected an object, got {j}")) };
            let mut fields = Vec::new();
            for (k, v) in o {
                let Some(i) = s.field_index(k) else {
                    let names: Vec<&str> = s.fields().iter().map(|f| f.name).collect();
                    return Err(format!("{}: unknown field (fields: {})", at(k), names.join(", ")));
                };
                if s.fields()[i].attrs.readonly {
                    return Err(format!("{}: field is read-only", at(k)));
                }
                fields.push((k.clone(), json_to_value(v, s.field(i), db, &at(k))?));
            }
            Value::Struct(fields)
        }
        ReflectRef::Enum(e) => {
            let Json::String(name) = j else { return Err(format!("{here}: expected a variant name, got {j}")) };
            match e.variants().iter().find(|v| v.eq_ignore_ascii_case(name)) {
                Some(v) => Value::Enum(v.to_string()),
                None => return Err(format!("{here}: no variant `{name}` (variants: {})", e.variants().join(", "))),
            }
        }
        ReflectRef::List(l) => {
            let Json::Array(items) = j else { return Err(format!("{here}: expected an array, got {j}")) };
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                // Existing items (or the first one) give the element shape.
                let shape = (!l.is_empty()).then(|| l.get(i.min(l.len() - 1)));
                out.push(match shape {
                    Some(r) => json_to_value(item, r, db, &at(&i.to_string()))?,
                    None => untyped(item),
                });
            }
            Value::List(out)
        }
        ReflectRef::Bool(_) => match j {
            Json::Bool(b) => Value::Bool(*b),
            _ => return Err(format!("{here}: expected true/false, got {j}")),
        },
        ReflectRef::I32(_) | ReflectRef::U32(_) | ReflectRef::I64(_) | ReflectRef::U64(_) => Value::Int(num(j, here)?.round() as i64),
        ReflectRef::F32(_) | ReflectRef::F64(_) => Value::Float(num(j, here)?),
        ReflectRef::String(_) => match j {
            Json::String(s) => Value::String(s.clone()),
            _ => return Err(format!("{here}: expected a string, got {j}")),
        },
        ReflectRef::Vec2(_) => vector(j, 2, false, here)?,
        ReflectRef::Vec3(_) => vector(j, 3, false, here)?,
        ReflectRef::Vec4(_) => vector(j, 4, false, here)?,
        ReflectRef::Quat(_) => vector(j, 4, false, here)?,
        ReflectRef::Color(_) => vector(j, 4, true, here)?,
        ReflectRef::Asset(_) => Value::Asset(resolve_asset(j, db).map_err(|e| format!("{here}: {e}"))?),
        ReflectRef::Entity(_) => match j {
            Json::Null => Value::Entity(Entity::NONE.to_bits()),
            Json::Number(n) => Value::Entity(n.as_u64().ok_or_else(|| format!("{here}: bad entity id"))?),
            _ => return Err(format!("{here}: expected an entity id or null, got {j}")),
        },
        ReflectRef::Opaque(_) => return Err(format!("{here}: this field cannot be edited")),
    })
}

/// Compact type description of a reflected value, for `list_component_types`.
pub fn schema(r: &dyn Reflect) -> Json {
    match r.reflect_ref() {
        ReflectRef::Struct(s) => {
            let mut m = Map::new();
            for (i, f) in s.fields().iter().enumerate() {
                let mut t = schema(s.field(i));
                let a = &f.attrs;
                if let Json::Object(o) = &mut t {
                    if let Some((lo, hi)) = a.range {
                        o.insert("range".into(), json!([lo, hi]));
                    }
                    if let Some(tip) = a.tooltip {
                        o.insert("doc".into(), json!(tip));
                    }
                    if a.readonly {
                        o.insert("readonly".into(), json!(true));
                    }
                    if let Some(k) = a.asset_kind {
                        o.insert("asset_kind".into(), json!(k));
                    }
                }
                m.insert(f.name.to_string(), t);
            }
            json!({ "type": "object", "fields": m })
        }
        ReflectRef::Enum(e) => json!({ "type": "enum", "variants": e.variants() }),
        ReflectRef::List(l) => json!({ "type": "list", "items": if !l.is_empty() { schema(l.get(0)) } else { json!("any") } }),
        ReflectRef::Bool(_) => json!({ "type": "bool" }),
        ReflectRef::I32(_) | ReflectRef::I64(_) => json!({ "type": "int" }),
        ReflectRef::U32(_) | ReflectRef::U64(_) => json!({ "type": "uint" }),
        ReflectRef::F32(_) | ReflectRef::F64(_) => json!({ "type": "float" }),
        ReflectRef::String(_) => json!({ "type": "string" }),
        ReflectRef::Vec2(_) => json!({ "type": "vec2" }),
        ReflectRef::Vec3(_) => json!({ "type": "vec3" }),
        ReflectRef::Vec4(_) => json!({ "type": "vec4" }),
        ReflectRef::Quat(_) => json!({ "type": "quat [x, y, z, w]" }),
        ReflectRef::Color(_) => json!({ "type": "color [r, g, b, a] linear" }),
        ReflectRef::Asset(_) => json!({ "type": "asset (path or UUID)" }),
        ReflectRef::Entity(_) => json!({ "type": "entity id" }),
        ReflectRef::Opaque(_) => json!({ "type": "opaque" }),
    }
}

/// Merge `patch` into `base` (objects merge recursively, everything else replaces).
pub fn merge(base: &mut Json, patch: &Json) {
    match (base, patch) {
        (Json::Object(b), Json::Object(p)) => {
            for (k, v) in p {
                match b.get_mut(k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        b.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (b, p) => *b = p.clone(),
    }
}

/// Apply a JSON patch to a serde type, rejecting fields it does not have.
pub fn patch_serde<T: serde::Serialize + serde::de::DeserializeOwned>(value: &T, patch: &Json) -> Result<T, String> {
    let mut base = serde_json::to_value(value).map_err(|e| e.to_string())?;
    check_keys(&base, patch, "")?;
    merge(&mut base, patch);
    serde_json::from_value(base).map_err(|e| e.to_string())
}

fn check_keys(base: &Json, patch: &Json, path: &str) -> Result<(), String> {
    if let (Json::Object(b), Json::Object(p)) = (base, patch) {
        for (k, v) in p {
            let at = if path.is_empty() { k.clone() } else { format!("{path}.{k}") };
            match b.get(k) {
                Some(bv) => check_keys(bv, v, &at)?,
                None => {
                    let names: Vec<&String> = b.keys().collect();
                    return Err(format!("{at}: unknown setting (known: {})", names.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dumb_ecs::{Light, LightKind, Transform};
    use dumb_reflect::apply;

    fn db() -> (AssetDatabase, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("dumb_mcp_convert_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("Assets")).unwrap();
        (AssetDatabase::open(&dir).unwrap(), dir)
    }

    #[test]
    fn round_trips_a_component() {
        let (db, dir) = db();
        let t = Transform { translation: dumb_core::Vec3::new(1.0, 2.0, 3.0), ..Default::default() };
        let j = reflect_to_json(&t, &db);
        assert_eq!(j["translation"], json!([1.0, 2.0, 3.0]));
        let mut u = Transform::default();
        let v = json_to_value(&j, &u, &db, "").unwrap();
        apply(&mut u, &v);
        assert_eq!(u.translation, t.translation);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn partial_update_keeps_other_fields() {
        let (db, dir) = db();
        let mut l = Light { kind: LightKind::Point, intensity: 3.0, ..Default::default() };
        let v = json_to_value(&json!({ "kind": "directional" }), &l, &db, "").unwrap();
        apply(&mut l, &v);
        assert_eq!(l.kind, LightKind::Directional);
        assert_eq!(l.intensity, 3.0);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn rejects_unknown_fields_and_variants() {
        let (db, dir) = db();
        let l = Light::default();
        let e = json_to_value(&json!({ "colour": [1, 0, 0] }), &l, &db, "").unwrap_err();
        assert!(e.contains("unknown field") && e.contains("color"), "{e}");
        let e = json_to_value(&json!({ "kind": "Spot" }), &l, &db, "").unwrap_err();
        assert!(e.contains("Point"), "{e}");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn colors_accept_rgb_and_assets_accept_builtins() {
        let (db, dir) = db();
        let l = Light::default();
        assert_eq!(json_to_value(&json!({ "color": [1, 0.5, 0] }), &l, &db, "").unwrap(), Value::Struct(vec![("color".into(), Value::Vec(vec![1.0, 0.5, 0.0, 1.0]))]));
        assert_eq!(resolve_asset(&json!("cube"), &db).unwrap(), dumb_core::builtin::CUBE);
        assert_eq!(resolve_asset(&json!(null), &db).unwrap(), AssetId::NONE);
        assert!(resolve_asset(&json!("Nope/x.glb"), &db).is_err());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn serde_patch_checks_keys() {
        let s = crate::prefs::EditorSettings::default();
        let p = patch_serde(&s, &json!({ "show_grid": false })).unwrap();
        assert!(!p.show_grid);
        assert!(patch_serde(&s, &json!({ "show_gird": false })).is_err());
    }
}
