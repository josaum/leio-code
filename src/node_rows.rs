//! Local LEIO node entities and their Arrow `RecordBatch` encoding.
//!
//! [`visit_node_entities`] walks a [`RepoIndex`] and emits one JSON entity per
//! observable unit (files, symbols, env vars, redis keys, deploy targets,
//! cartridges, profiles, secret sets, API routes, docker services). Each entity
//! is scoped to a tenant/repository/ revision primary key
//! ([`scoped_node_id`]) so stores never collide across tenants or revisions.
//! [`build_leio_row_batch`] encodes a slice of those entities into the Arrow
//! IPC schema consumed by `.leio-code/exports/arrow-nodes-v1/nodes.arrow` and
//! the local cosine/lexical search sidecar.

use std::collections::HashMap;
use std::env;
use std::path::Path;
use std::sync::Arc;

use arrow_array::builder::{
    BinaryBuilder, Float32Builder, Int64Builder, ListBuilder, StringBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType as ArrowDataType, Field as ArrowField, Schema as ArrowSchema};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::embed::{EMBED_DIM, EMBED_MODEL};
use crate::model::RepoIndex;
use crate::query::{
    AggregatedApiRoute, DockerServiceCandidate, collect_api_routes, collect_docker_services,
};

/// Dimension of the `code_vec` float arm (identifier/structural view).
const CODE_VEC_DIM: usize = 1536;
/// Dimension of the `semantic_vec` float arm (natural-language view).
const SEMANTIC_VEC_DIM: usize = 1536;
/// Dimension of the `ontology_vec` float arm (ontology/context view).
const ONTOLOGY_VEC_DIM: usize = 768;
/// Compute a SimHash fingerprint for a text string to a 512-bit binary vector.
///
/// Each bit is the sign of the weighted hash for that dimension.
fn simhash_512(text: &str) -> Vec<u8> {
    let mut counts = vec![0i32; 512];
    let words: Vec<&str> = text.split_whitespace().collect();

    for word in &words {
        let mut hasher = Sha256::new();
        hasher.update(word.as_bytes());
        let hash = hasher.finalize();

        // Expand 256-bit SHA-256 to 512 bits by hashing with a salt.
        let mut hasher2 = Sha256::new();
        hasher2.update(b"salt:");
        hasher2.update(word.as_bytes());
        let hash2 = hasher2.finalize();

        let combined: Vec<u8> = hash.iter().chain(hash2.iter()).copied().collect();

        for (i, &byte) in combined.iter().enumerate() {
            for bit in 0..8 {
                let dim = i * 8 + bit;
                if dim >= 512 {
                    break;
                }
                if (byte >> bit) & 1 == 1 {
                    counts[dim] += 1;
                } else {
                    counts[dim] -= 1;
                }
            }
        }
    }

    let mut result = vec![0u8; 64];
    for (i, &count) in counts.iter().enumerate() {
        if count > 0 {
            result[i / 8] |= 1 << (i % 8);
        }
    }
    result
}

fn hex_encode(data: &[u8]) -> String {
    data.iter().map(|b| format!("{:02x}", b)).collect()
}

fn node_tenant_id(repo_root: &Path) -> String {
    env::var("LEIO_CODE_TENANT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            if crate::config::repo_profile(repo_root) == "example" {
                "workspace".to_string()
            } else {
                crate::config::repo_namespace(repo_root)
            }
        })
}

fn node_repo_id(repo_root: &Path) -> String {
    env::var("LEIO_CODE_REPO")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| crate::config::repo_namespace(repo_root))
}

fn node_revision() -> String {
    env::var("LEIO_CODE_REV")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "local".to_string())
}

/// Opaque, bounded primary-key prefix for one tenant/repository/revision scope.
///
/// Store primary keys are collection-global, even when `tenant_id` is a
/// partition key. Hashing the complete scope prevents two tenants with the
/// same repository-relative logical ID from overwriting each other while
/// keeping raw tenant and repository identifiers out of the primary key.
fn node_scope_prefix(repo_root: &Path) -> String {
    node_scope_prefix_for_revision(repo_root, &node_revision())
}

fn node_scope_prefix_for_revision(repo_root: &Path, revision: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(node_tenant_id(repo_root).as_bytes());
    hasher.update([0]);
    hasher.update(node_repo_id(repo_root).as_bytes());
    hasher.update([0]);
    hasher.update(revision.as_bytes());
    format!("{}::", hex_encode(&hasher.finalize()))
}

fn scoped_node_id(repo_root: &Path, logical_id: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(logical_id.as_bytes());
    format!(
        "{}{}",
        node_scope_prefix(repo_root),
        hex_encode(&hasher.finalize())
    )
}

pub(crate) fn api_route_logical_id(route: &AggregatedApiRoute) -> String {
    format!(
        "api_route:{}:{}:{}:{}",
        route.file_path, route.line, route.route_role, route.full_path
    )
}

pub(crate) fn docker_service_logical_id(service: &DockerServiceCandidate) -> String {
    format!("docker_service:{}:{}", service.file_path, service.name)
}

fn path_cartridge_name(path: &str) -> Option<&str> {
    let mut segments = path.split('/');
    match (segments.next(), segments.next()) {
        (Some("cartridges"), Some(name)) if !name.is_empty() && !name.contains('.') => Some(name),
        _ => None,
    }
}

fn parse_csv_list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(|item| item.to_string())
        .collect()
}

fn profile_active_cartridges(index: &RepoIndex) -> HashMap<String, Vec<String>> {
    let mut by_profile = HashMap::new();
    for profile in &index.profiles {
        let cartridges = profile
            .vars
            .iter()
            .find(|var| var.name == "EXAMPLE_ACTIVE_CARTRIDGES")
            .and_then(|var| var.value_preview.as_deref())
            .map(parse_csv_list)
            .unwrap_or_default();
        by_profile.insert(profile.name.clone(), cartridges);
    }
    by_profile
}

fn cartridge_activation_profiles(index: &RepoIndex) -> HashMap<String, Vec<String>> {
    let mut profiles_by_cartridge: HashMap<String, Vec<String>> = HashMap::new();
    for (profile_name, cartridges) in profile_active_cartridges(index) {
        for cartridge in cartridges {
            profiles_by_cartridge
                .entry(cartridge)
                .or_default()
                .push(profile_name.clone());
        }
    }
    profiles_by_cartridge
}

fn merge_metadata(logical_id: &str, extra: Value) -> Value {
    let mut metadata = serde_json::Map::new();
    metadata.insert("logical_id".to_string(), json!(logical_id));
    if let Some(extra_obj) = extra.as_object() {
        for (key, value) in extra_obj {
            metadata.insert(key.clone(), value.clone());
        }
    }
    Value::Object(metadata)
}

fn annotate_entity(entity: &mut Value, relations: Vec<String>, metadata: Value) {
    if let Some(obj) = entity.as_object_mut() {
        obj.insert("relations".to_string(), json!(relations));
        obj.insert("metadata".to_string(), metadata);
    }
}

/// Build the execution-trace fingerprint text from entity metadata.
///
/// The binary `execution_vec_bin` SimHash arm is orthogonal to the semantic
/// float arms (it fingerprints structural identity for Hamming dedup), so it
/// keeps a deterministic text independent of the embedding views.
fn build_execution_text(kind: &str, name: &str, path: &str, lang: &str) -> String {
    format!("{name} {kind} {path} {lang}")
}

/// Build a node entity JSON from index data for the textual regime.
///
/// The three float vectors (`code_vec`/`semantic_vec`/`ontology_vec`) are
/// emitted as zero placeholders at the BGE-M3 contract dimension (`EMBED_DIM`)
/// and are replaced in a single batched pass by [`crate::embed::embed_node_entities`]
/// during export, using the remote encoder over three distinct text
/// views. If that encoder is unreachable the placeholders survive (graceful
/// degrade) and the row stays `embed_model: "none"`. Only `execution_vec_bin`
/// (binary Hamming fingerprint) is computed locally via SimHash.
#[allow(clippy::too_many_arguments)]
fn build_node_entity(
    repo_root: &Path,
    logical_node_id: &str,
    kind: &str,
    symbol: &str,
    path: &str,
    lang: &str,
    target: &str,
    text_snippet: &str,
) -> Value {
    let exec_bin = simhash_512(&build_execution_text(kind, symbol, path, lang))
        .into_iter()
        .map(|value| json!(value))
        .collect::<Vec<_>>();
    let tenant_id = node_tenant_id(repo_root);
    let repo_id = node_repo_id(repo_root);
    let revision = node_revision();
    let node_id = scoped_node_id(repo_root, logical_node_id);

    json!({
        "node_id": node_id,
        "tenant_id": tenant_id,
        "repo": repo_id,
        "rev": revision,
        "path": path,
        "lang": lang,
        "kind": kind,
        "symbol": symbol,
        "target": target,
        "relations": [],
        "metadata": {
            "logical_id": logical_node_id,
        },
        "text_snippet": &text_snippet[..text_snippet.len().min(2000)],
        "embed_model": "none",
        "embed_dim": 0,
        "code_vec": vec![0.0f64; EMBED_DIM],
        "semantic_vec": vec![0.0f64; EMBED_DIM],
        "ontology_vec": vec![0.0f64; EMBED_DIM],
        "execution_vec_bin": exec_bin,
    })
}

/// Walk `index` and emit one entity per observable unit to `on_entity`.
pub(crate) fn visit_node_entities<F>(index: &RepoIndex, repo_root: &Path, mut on_entity: F)
where
    F: FnMut(Value),
{
    let profile_cartridges = profile_active_cartridges(index);
    let cartridge_profiles = cartridge_activation_profiles(index);
    let routes = collect_api_routes(index);
    let docker_services = collect_docker_services(index);
    let mut cartridge_names: Vec<String> = index.cartridge_names().into_iter().collect();
    cartridge_names.sort();

    for file in &index.files {
        let file_id = format!("file:{}", file.path);
        let snippet = format!(
            "kind=file path={} lang={}",
            file.path,
            file.language.as_str()
        );
        let mut entity = build_node_entity(
            repo_root,
            &file_id,
            "file",
            "",
            &file.path,
            file.language.as_str(),
            "",
            &snippet,
        );
        let mut relations = vec![
            "kind:file".to_string(),
            format!("lang:{}", file.language.as_str()),
        ];
        if let Some(cartridge) = path_cartridge_name(&file.path) {
            relations.push(format!("cartridge:{cartridge}"));
        }
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &file_id,
                json!({
                    "source_bytes": file.bytes,
                    "modified_unix_ms": file.modified_unix_ms,
                }),
            ),
        );
        on_entity(entity);

        for sym in &file.symbols {
            let logical_id = format!(
                "symbol:{}:{}:{}:{}",
                file.path,
                sym.line,
                sym.kind.as_str(),
                sym.name,
            );
            let snippet = format!(
                "kind={} label={} path={} lang={}",
                sym.kind.as_str(),
                sym.name,
                file.path,
                file.language.as_str(),
            );
            let mut entity = build_node_entity(
                repo_root,
                &logical_id,
                "symbol",
                &sym.name,
                &file.path,
                file.language.as_str(),
                "",
                &snippet,
            );
            let mut relations = vec![
                "kind:symbol".to_string(),
                format!("lang:{}", file.language.as_str()),
                format!("symbol_kind:{}", sym.kind.as_str()),
            ];
            if let Some(cartridge) = path_cartridge_name(&file.path) {
                relations.push(format!("cartridge:{cartridge}"));
            }
            annotate_entity(
                &mut entity,
                relations,
                merge_metadata(
                    &logical_id,
                    json!({
                        "line": sym.line,
                        "symbol_kind": sym.kind.as_str(),
                    }),
                ),
            );
            on_entity(entity);
        }

        for ev in &file.env_vars {
            let logical_id = format!("env_var:{}:{}:{}", file.path, ev.line, ev.name);
            let snippet = format!(
                "kind=env_var label={} access={:?} path={}",
                ev.name, ev.access, file.path,
            );
            let mut entity = build_node_entity(
                repo_root,
                &logical_id,
                "env_var",
                &ev.name,
                &file.path,
                file.language.as_str(),
                "",
                &snippet,
            );
            let mut relations = vec![
                "kind:env_var".to_string(),
                format!("access:{}", ev.access.as_str()),
                format!("lang:{}", file.language.as_str()),
            ];
            if let Some(cartridge) = path_cartridge_name(&file.path) {
                relations.push(format!("cartridge:{cartridge}"));
            }
            annotate_entity(
                &mut entity,
                relations,
                merge_metadata(
                    &logical_id,
                    json!({
                        "line": ev.line,
                        "access": ev.access.as_str(),
                    }),
                ),
            );
            on_entity(entity);
        }

        for rk in &file.redis_keys {
            let logical_id = format!("redis_key:{}:{}:{}", file.path, rk.line, rk.key);
            let snippet = format!(
                "kind=redis_key label={} access={:?} path={}",
                rk.key, rk.access, file.path,
            );
            let mut entity = build_node_entity(
                repo_root,
                &logical_id,
                "redis_key",
                &rk.key,
                &file.path,
                file.language.as_str(),
                "",
                &snippet,
            );
            let mut relations = vec![
                "kind:redis_key".to_string(),
                format!("access:{}", rk.access.as_str()),
                format!("lang:{}", file.language.as_str()),
            ];
            if let Some(cartridge) = path_cartridge_name(&file.path) {
                relations.push(format!("cartridge:{cartridge}"));
            }
            annotate_entity(
                &mut entity,
                relations,
                merge_metadata(
                    &logical_id,
                    json!({
                        "line": rk.line,
                        "access": rk.access.as_str(),
                    }),
                ),
            );
            on_entity(entity);
        }
    }

    for dt in &index.deploy_targets {
        let logical_id = format!("deploy_target:{}", dt.name);
        let cartridges = dt.cartridges.join(", ");
        let snippet = format!(
            "kind=deploy_target label={} topology={} cartridges={}",
            dt.name,
            dt.topology.as_deref().unwrap_or(""),
            cartridges,
        );
        let mut entity = build_node_entity(
            repo_root,
            &logical_id,
            "deploy_target",
            &dt.name,
            &dt.path,
            "toml",
            &dt.name,
            &snippet,
        );
        let mut relations = vec!["kind:deploy_target".to_string()];
        relations.extend(dt.cartridges.iter().map(|item| format!("cartridge:{item}")));
        relations.extend(
            dt.required_integrations
                .iter()
                .map(|item| format!("integration:{item}")),
        );
        if let Some(profile) = dt.profile.as_deref() {
            relations.push(format!("profile:{profile}"));
        }
        if let Some(secret_set) = dt.secret_set.as_deref() {
            relations.push(format!("secret_set:{secret_set}"));
        }
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &logical_id,
                json!({
                    "topology": dt.topology.clone(),
                    "profile": dt.profile.clone(),
                    "backend_profile": dt.backend_profile.clone(),
                    "secret_set": dt.secret_set.clone(),
                    "health_checks": dt.health_checks.clone(),
                    "required_integrations": dt.required_integrations.clone(),
                    "cartridges": dt.cartridges.clone(),
                }),
            ),
        );
        on_entity(entity);
    }

    for cartridge in cartridge_names {
        let logical_id = format!("cartridge:{cartridge}");
        let prefix = format!("cartridges/{cartridge}/");
        let source_file_count = index
            .files
            .iter()
            .filter(|file| file.path.starts_with(&prefix))
            .count();
        let deploy_targets: Vec<String> = index
            .deploy_targets
            .iter()
            .filter(|target| target.cartridges.iter().any(|item| item == &cartridge))
            .map(|target| target.name.clone())
            .collect();
        let activation_profiles = cartridge_profiles
            .get(&cartridge)
            .cloned()
            .unwrap_or_default();
        let integrations: Vec<String> = index
            .deploy_targets
            .iter()
            .filter(|target| target.cartridges.iter().any(|item| item == &cartridge))
            .flat_map(|target| target.required_integrations.iter().cloned())
            .collect();
        let snippet = format!(
            "kind=cartridge label={} source_files={} deploy_targets={} activation_profiles={} integrations={}",
            cartridge,
            source_file_count,
            deploy_targets.join(","),
            activation_profiles.join(","),
            integrations.join(","),
        );
        let mut entity = build_node_entity(
            repo_root,
            &logical_id,
            "cartridge",
            &cartridge,
            &format!("cartridges/{cartridge}"),
            "text",
            "",
            &snippet,
        );
        let mut relations = vec!["kind:cartridge".to_string()];
        let mut deploy_targets = deploy_targets;
        deploy_targets.sort();
        deploy_targets.dedup();
        let mut activation_profiles = activation_profiles;
        activation_profiles.sort();
        activation_profiles.dedup();
        let mut integrations = integrations;
        integrations.sort();
        integrations.dedup();
        relations.extend(
            deploy_targets
                .iter()
                .map(|item| format!("deploy_target:{item}")),
        );
        relations.extend(
            activation_profiles
                .iter()
                .map(|item| format!("profile:{item}")),
        );
        relations.extend(
            integrations
                .iter()
                .map(|item| format!("integration:{item}")),
        );
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &logical_id,
                json!({
                    "source_file_count": source_file_count,
                    "deploy_targets": deploy_targets,
                    "activation_profiles": activation_profiles,
                    "required_integrations": integrations,
                }),
            ),
        );
        on_entity(entity);
    }

    for profile in &index.profiles {
        let logical_id = format!("profile:{}", profile.name);
        let active_cartridges = profile_cartridges
            .get(&profile.name)
            .cloned()
            .unwrap_or_default();
        let declared_vars: Vec<String> = profile.vars.iter().map(|var| var.name.clone()).collect();
        let snippet = format!(
            "kind=profile label={} declared_vars={} active_cartridges={}",
            profile.name,
            declared_vars.join(","),
            active_cartridges.join(","),
        );
        let mut entity = build_node_entity(
            repo_root,
            &logical_id,
            "profile",
            &profile.name,
            &profile.path,
            "env",
            "",
            &snippet,
        );
        let mut relations = vec!["kind:profile".to_string()];
        relations.extend(
            declared_vars
                .iter()
                .map(|item| format!("declares_env:{item}")),
        );
        relations.extend(
            active_cartridges
                .iter()
                .map(|item| format!("cartridge:{item}")),
        );
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &logical_id,
                json!({
                    "declared_vars": declared_vars,
                    "var_count": profile.vars.len(),
                    "active_cartridges": active_cartridges,
                }),
            ),
        );
        on_entity(entity);
    }

    for secret_set in &index.secret_sets {
        let logical_id = format!("secret_set:{}", secret_set.name);
        let declared_vars: Vec<String> =
            secret_set.vars.iter().map(|var| var.name.clone()).collect();
        let snippet = format!(
            "kind=secret_set label={} declared_vars={}",
            secret_set.name,
            declared_vars.join(","),
        );
        let mut entity = build_node_entity(
            repo_root,
            &logical_id,
            "secret_set",
            &secret_set.name,
            &secret_set.path,
            "env",
            "",
            &snippet,
        );
        let mut relations = vec!["kind:secret_set".to_string()];
        relations.extend(
            declared_vars
                .iter()
                .map(|item| format!("declares_env:{item}")),
        );
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &logical_id,
                json!({
                    "declared_vars": declared_vars,
                    "var_count": secret_set.vars.len(),
                }),
            ),
        );
        on_entity(entity);
    }

    for route in &routes {
        let logical_id = api_route_logical_id(route);
        let methods = route.methods.join(", ");
        let handlers = route.handlers.join(", ");
        let activation_profiles = route.activation_profiles.clone().unwrap_or_default();
        let snippet = format!(
            "kind=api_route label={} methods={} handlers={} family={} mount_status={} auth_policy={} route_role={} activation_profiles={}",
            route.full_path,
            methods,
            handlers,
            route.route_family,
            route.mount_status,
            route.auth_policy,
            route.route_role,
            activation_profiles.join(","),
        );
        let mut entity = build_node_entity(
            repo_root,
            &logical_id,
            "api_route",
            &route.full_path,
            &route.file_path,
            &route.language,
            &route.route_family,
            &snippet,
        );
        let mut relations = vec![
            "kind:api_route".to_string(),
            format!("route_family:{}", route.route_family),
            format!("mount_status:{}", route.mount_status),
            format!("auth_policy:{}", route.auth_policy),
            format!("route_role:{}", route.route_role),
        ];
        relations.extend(route.methods.iter().map(|item| format!("method:{item}")));
        relations.extend(
            activation_profiles
                .iter()
                .map(|item| format!("profile:{item}")),
        );
        if let Some(public) = route.public {
            relations.push(format!("public:{public}"));
        }
        if let Some(cartridge) = path_cartridge_name(&route.file_path) {
            relations.push(format!("cartridge:{cartridge}"));
        }
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &logical_id,
                json!({
                    "methods": route.methods,
                    "handlers": route.handlers,
                    "mounted": route.mounted,
                    "mount_status": route.mount_status,
                    "activation_profiles": activation_profiles,
                    "public": route.public,
                    "auth_policy": route.auth_policy,
                    "auth_source": route.auth_source,
                    "route_role": route.route_role,
                    "canonical_path": route.canonical_path,
                    "route_family": route.route_family,
                    "route_family_label": route.route_family_label,
                    "line": route.line,
                }),
            ),
        );
        on_entity(entity);
    }

    for service in &docker_services {
        let logical_id = docker_service_logical_id(service);
        let snippet = format!(
            "kind=docker_service label={} image={} build_context={} profiles={}",
            service.name,
            service.image.clone().unwrap_or_default(),
            service.build_context.clone().unwrap_or_default(),
            service.profiles.join(","),
        );
        let mut entity = build_node_entity(
            repo_root,
            &logical_id,
            "docker_service",
            &service.name,
            &service.file_path,
            "yaml",
            "",
            &snippet,
        );
        let mut relations = vec!["kind:docker_service".to_string()];
        relations.extend(
            service
                .profiles
                .iter()
                .map(|item| format!("profile:{item}")),
        );
        annotate_entity(
            &mut entity,
            relations,
            merge_metadata(
                &logical_id,
                json!({
                    "image": service.image,
                    "build_context": service.build_context,
                    "profiles": service.profiles,
                }),
            ),
        );
        on_entity(entity);
    }
}

struct BatchCapacity {
    rows: usize,
    string_bytes: usize,
    relation_items: usize,
    execution_bytes: usize,
}

fn estimate_batch_capacity(entities: &[Value]) -> Result<BatchCapacity, String> {
    let rows = entities.len();
    let mut string_bytes = 0usize;
    let mut relation_items = 0usize;
    let mut execution_bytes = 0usize;

    for entity in entities {
        let object = entity
            .as_object()
            .ok_or_else(|| "node entity was not a JSON object".to_string())?;
        for key in [
            "node_id",
            "tenant_id",
            "repo",
            "rev",
            "path",
            "lang",
            "kind",
            "symbol",
            "target",
            "text_snippet",
            "embed_model",
        ] {
            string_bytes = string_bytes.saturating_add(value_as_str(object.get(key))?.len());
        }
        if let Some(metadata) = object.get("metadata") {
            string_bytes = string_bytes.saturating_add(
                serde_json::to_string(metadata)
                    .map_err(|err| format!("failed to encode metadata JSON: {err}"))?
                    .len(),
            );
        }
        let relations = value_as_array(object.get("relations"))?;
        relation_items = relation_items.saturating_add(relations.len());
        for relation in relations {
            string_bytes = string_bytes.saturating_add(
                relation
                    .as_str()
                    .ok_or_else(|| "relation entry was not a string".to_string())?
                    .len(),
            );
        }
        execution_bytes =
            execution_bytes.saturating_add(value_as_array(object.get("execution_vec_bin"))?.len());
    }

    Ok(BatchCapacity {
        rows,
        string_bytes,
        relation_items,
        execution_bytes,
    })
}

pub(crate) fn build_leio_row_batch(entities: &[Value]) -> Result<RecordBatch, String> {
    let schema = ArrowSchema::new(vec![
        ArrowField::new("node_id", ArrowDataType::Utf8, false),
        ArrowField::new("tenant_id", ArrowDataType::Utf8, false),
        ArrowField::new("repo", ArrowDataType::Utf8, false),
        ArrowField::new("rev", ArrowDataType::Utf8, false),
        ArrowField::new("path", ArrowDataType::Utf8, false),
        ArrowField::new("lang", ArrowDataType::Utf8, false),
        ArrowField::new("kind", ArrowDataType::Utf8, false),
        ArrowField::new("symbol", ArrowDataType::Utf8, false),
        ArrowField::new("target", ArrowDataType::Utf8, false),
        ArrowField::new(
            "relations",
            ArrowDataType::List(Arc::new(ArrowField::new("item", ArrowDataType::Utf8, true))),
            true,
        ),
        ArrowField::new("metadata", ArrowDataType::Utf8, true),
        ArrowField::new("text_snippet", ArrowDataType::Utf8, false),
        ArrowField::new("embed_model", ArrowDataType::Utf8, false),
        ArrowField::new("embed_dim", ArrowDataType::Int64, false),
        ArrowField::new(
            "code_vec",
            ArrowDataType::List(Arc::new(ArrowField::new(
                "item",
                ArrowDataType::Float32,
                true,
            ))),
            false,
        ),
        ArrowField::new(
            "semantic_vec",
            ArrowDataType::List(Arc::new(ArrowField::new(
                "item",
                ArrowDataType::Float32,
                true,
            ))),
            false,
        ),
        ArrowField::new(
            "ontology_vec",
            ArrowDataType::List(Arc::new(ArrowField::new(
                "item",
                ArrowDataType::Float32,
                true,
            ))),
            false,
        ),
        ArrowField::new("execution_vec_bin", ArrowDataType::Binary, false),
    ]);

    let capacity = estimate_batch_capacity(entities)?;
    let string_data_bytes = capacity.string_bytes.max(capacity.rows.saturating_mul(64));

    let mut node_id_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut tenant_id_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut repo_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut rev_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut path_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut lang_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut kind_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut symbol_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut target_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut relations_builder = ListBuilder::with_capacity(
        StringBuilder::with_capacity(
            capacity.relation_items,
            capacity.relation_items.saturating_mul(32),
        ),
        capacity.rows,
    );
    let mut metadata_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut text_snippet_builder = StringBuilder::with_capacity(capacity.rows, string_data_bytes);
    let mut embed_model_builder = StringBuilder::with_capacity(
        capacity.rows,
        capacity.rows.saturating_mul(EMBED_MODEL.len().max(8)),
    );
    let mut embed_dim_builder = Int64Builder::with_capacity(capacity.rows);
    let mut code_vec_builder = ListBuilder::with_capacity(
        Float32Builder::with_capacity(capacity.rows.saturating_mul(CODE_VEC_DIM)),
        capacity.rows,
    );
    let mut semantic_vec_builder = ListBuilder::with_capacity(
        Float32Builder::with_capacity(capacity.rows.saturating_mul(SEMANTIC_VEC_DIM)),
        capacity.rows,
    );
    let mut ontology_vec_builder = ListBuilder::with_capacity(
        Float32Builder::with_capacity(capacity.rows.saturating_mul(ONTOLOGY_VEC_DIM)),
        capacity.rows,
    );
    let mut execution_vec_builder =
        BinaryBuilder::with_capacity(capacity.rows, capacity.execution_bytes);

    for entity in entities {
        let object = entity
            .as_object()
            .ok_or_else(|| "node entity was not a JSON object".to_string())?;
        node_id_builder.append_value(value_as_str(object.get("node_id"))?);
        tenant_id_builder.append_value(value_as_str(object.get("tenant_id"))?);
        repo_builder.append_value(value_as_str(object.get("repo"))?);
        rev_builder.append_value(value_as_str(object.get("rev"))?);
        path_builder.append_value(value_as_str(object.get("path"))?);
        lang_builder.append_value(value_as_str(object.get("lang"))?);
        kind_builder.append_value(value_as_str(object.get("kind"))?);
        symbol_builder.append_value(value_as_str(object.get("symbol"))?);
        target_builder.append_value(value_as_str(object.get("target"))?);

        let relations = value_as_array(object.get("relations"))?;
        for relation in relations {
            relations_builder.values().append_value(
                relation
                    .as_str()
                    .ok_or_else(|| "relation entry was not a string".to_string())?,
            );
        }
        relations_builder.append(true);

        metadata_builder.append_value(
            serde_json::to_string(object.get("metadata").unwrap_or(&Value::Null))
                .map_err(|err| format!("failed to encode metadata JSON: {err}"))?,
        );
        text_snippet_builder.append_value(value_as_str(object.get("text_snippet"))?);

        // `embed_model`/`embed_dim` are stamped only on rows that were embedded
        // by the remote encoder. Rows that fell through the graceful-degrade
        // path carry an explicit "none"/0 marker rather than a false claim.
        embed_model_builder.append_value(
            object
                .get("embed_model")
                .and_then(Value::as_str)
                .unwrap_or("none"),
        );
        embed_dim_builder
            .append_value(object.get("embed_dim").and_then(Value::as_i64).unwrap_or(0));

        append_float_list(
            &mut code_vec_builder,
            value_as_array(object.get("code_vec"))?,
        )?;
        append_float_list(
            &mut semantic_vec_builder,
            value_as_array(object.get("semantic_vec"))?,
        )?;
        append_float_list(
            &mut ontology_vec_builder,
            value_as_array(object.get("ontology_vec"))?,
        )?;
        execution_vec_builder.append_value(&bytes_from_json_array(value_as_array(
            object.get("execution_vec_bin"),
        )?)?);
    }

    let columns: Vec<ArrayRef> = vec![
        Arc::new(node_id_builder.finish()),
        Arc::new(tenant_id_builder.finish()),
        Arc::new(repo_builder.finish()),
        Arc::new(rev_builder.finish()),
        Arc::new(path_builder.finish()),
        Arc::new(lang_builder.finish()),
        Arc::new(kind_builder.finish()),
        Arc::new(symbol_builder.finish()),
        Arc::new(target_builder.finish()),
        Arc::new(relations_builder.finish()),
        Arc::new(metadata_builder.finish()),
        Arc::new(text_snippet_builder.finish()),
        Arc::new(embed_model_builder.finish()),
        Arc::new(embed_dim_builder.finish()),
        Arc::new(code_vec_builder.finish()),
        Arc::new(semantic_vec_builder.finish()),
        Arc::new(ontology_vec_builder.finish()),
        Arc::new(execution_vec_builder.finish()),
    ];

    RecordBatch::try_new(Arc::new(schema), columns)
        .map_err(|err| format!("failed to build Arrow record batch: {err}"))
}

fn append_float_list(
    builder: &mut ListBuilder<Float32Builder>,
    values: &[Value],
) -> Result<(), String> {
    for value in values {
        builder.values().append_value(json_number_as_f32(value)?);
    }
    builder.append(true);
    Ok(())
}

fn value_as_str(value: Option<&Value>) -> Result<&str, String> {
    value
        .and_then(Value::as_str)
        .ok_or_else(|| "expected string field on node entity".to_string())
}

fn value_as_array(value: Option<&Value>) -> Result<&[Value], String> {
    value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| "expected array field on node entity".to_string())
}

fn json_number_as_f32(value: &Value) -> Result<f32, String> {
    value
        .as_f64()
        .map(|number| number as f32)
        .ok_or_else(|| "expected numeric vector entry".to_string())
}

fn bytes_from_json_array(values: &[Value]) -> Result<Vec<u8>, String> {
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|number| u8::try_from(number).ok())
                .ok_or_else(|| "expected execution_vec_bin to be an array of bytes".to_string())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Env mutation in these tests is unsound under parallel test threads, so
    /// every scoping test serializes on this lock and restores prior values.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct EnvRestore {
        tenant: Option<String>,
        repo: Option<String>,
        rev: Option<String>,
    }

    impl EnvRestore {
        fn capture() -> Self {
            Self {
                tenant: std::env::var("LEIO_CODE_TENANT_ID").ok(),
                repo: std::env::var("LEIO_CODE_REPO").ok(),
                rev: std::env::var("LEIO_CODE_REV").ok(),
            }
        }

        fn restore(self) {
            // SAFETY: serialized test-only process environment mutation.
            unsafe {
                match self.tenant {
                    Some(value) => std::env::set_var("LEIO_CODE_TENANT_ID", value),
                    None => std::env::remove_var("LEIO_CODE_TENANT_ID"),
                }
                match self.repo {
                    Some(value) => std::env::set_var("LEIO_CODE_REPO", value),
                    None => std::env::remove_var("LEIO_CODE_REPO"),
                }
                match self.rev {
                    Some(value) => std::env::set_var("LEIO_CODE_REV", value),
                    None => std::env::remove_var("LEIO_CODE_REV"),
                }
            }
        }
    }

    #[test]
    fn build_node_entity_scopes_primary_key_and_preserves_logical_id() {
        let _guard = ENV_LOCK.lock().unwrap();
        let restore = EnvRestore::capture();
        let entity = build_node_entity(
            Path::new("/tmp/demo-repo"),
            "file:src/main.rs",
            "file",
            "",
            "src/main.rs",
            "rust",
            "",
            "kind=file path=src/main.rs lang=rust",
        );
        let node_id = entity
            .get("node_id")
            .and_then(|value| value.as_str())
            .expect("node id");
        assert!(node_id.starts_with(&node_scope_prefix(Path::new("/tmp/demo-repo"))));
        assert_eq!(node_id.len(), 130, "scope hash + separator + logical hash");
        assert_eq!(
            entity
                .pointer("/metadata/logical_id")
                .and_then(|value| value.as_str()),
            Some("file:src/main.rs")
        );
        restore.restore();
    }

    #[test]
    fn build_node_entity_isolates_same_repo_across_tenants_and_revisions() {
        let _guard = ENV_LOCK.lock().unwrap();
        let restore = EnvRestore::capture();

        let build = |tenant: &str, revision: &str| {
            // SAFETY: serialized test-only process environment mutation.
            unsafe {
                std::env::set_var("LEIO_CODE_TENANT_ID", tenant);
                std::env::set_var("LEIO_CODE_REPO", "shared-repo");
                std::env::set_var("LEIO_CODE_REV", revision);
            }
            build_node_entity(
                Path::new("/tmp/shared-repo"),
                "file:src/main.rs",
                "file",
                "",
                "src/main.rs",
                "rust",
                "",
                "kind=file path=src/main.rs lang=rust",
            )
        };

        let tenant_a_main = build("tenant-a", "main");
        let tenant_b_main = build("tenant-b", "main");
        let tenant_a_feature = build("tenant-a", "feature");

        assert_eq!(tenant_a_main["tenant_id"], "tenant-a");
        assert_eq!(tenant_a_main["repo"], "shared-repo");
        assert_eq!(tenant_a_main["rev"], "main");
        assert_ne!(tenant_a_main["node_id"], tenant_b_main["node_id"]);
        assert_ne!(tenant_a_main["node_id"], tenant_a_feature["node_id"]);

        restore.restore();
    }
}
