//! C4 GYM EVO integration contract doctor.

use std::path::Path;
use std::time::Instant;

use ignore::WalkBuilder;
use serde_json::json;
use tree_sitter::{Node, Parser, Tree};

use super::Doctor;
use super::source_scan::find_awaited_function_call_line;
use super::utils::{find_line, query_id, read_text};
use crate::model::{EvidenceItem, QueryEnvelope, RepoIndex};

pub struct C4GymEvoIntegrationDoctor;

impl Doctor for C4GymEvoIntegrationDoctor {
    fn name(&self) -> &'static str {
        "c4gym-evo-integration"
    }

    fn description(&self) -> &'static str {
        "Checks C4 GYM EVO tenant/unit scope, credential broker, payment authority, deployment secret contracts, and Rust WhatsApp egress wiring."
    }

    fn run(&self, _index: &RepoIndex, root: &Path) -> QueryEnvelope {
        doctor_c4gym_evo_integration(root)
    }
}

struct Contract<'a> {
    path: &'a str,
    label: &'a str,
    needles: &'a [&'a str],
}

const CONTRACTS: &[Contract<'_>] = &[
    Contract {
        path: "cartridges/jaipay/tenant_scope.py",
        label: "Redis tenant-to-gym scope resolver",
        needles: &[
            "def jaipay_scope_key",
            "def load_jaipay_gym_scope",
            "concrete tenant is required",
        ],
    },
    Contract {
        path: "example-api/example/routers/evo_credentials.py",
        label: "authenticated metadata-only EVO credential management",
        needles: &[
            "require_evo_credential_manager",
            "EvoCredentialMetadata",
            "response_model=EvoCredentialMetadata",
            "x-tenant-id",
        ],
    },
    Contract {
        path: "example-api/example/routers/v2/evo_credential_broker.py",
        label: "exact-scope EVO credential broker",
        needles: &[
            "_binding",
            "verify_evo_use_grant",
            "consume_evo_broker_grant",
            "verify_evo_broker_request",
            "production_kms",
        ],
    },
    Contract {
        path: "jai-pay/src/lib/evo-credential-broker.ts",
        label: "JAI Pay signed broker client",
        needles: &[
            "server-only",
            "hmacConfiguration",
            "signedPost",
            "resolveEvoCredential",
            "EVO_BROKER_JAIPAY_HMAC_KEY",
        ],
    },
    Contract {
        path: "jai-pay/src/lib/evo.ts",
        label: "unit-scoped EVO request entrypoint",
        needles: &[
            "tenantId",
            "gymId",
            "expectedBranchId",
            "loadEvoProviderAccount",
            "resolveEvoCredential",
        ],
    },
    Contract {
        path: "jai-pay/src/lib/evo-route-context.ts",
        label: "shared EVO route tenant, gym, and grant gate",
        needles: &[
            "requireInternalTenantScope(request)",
            "scope.gymIds.includes(gymId)",
            "readEvoCredentialGrant(request)",
            "getEvoRequestContext({ tenantId: scope.tenantId, gymId, operation, requestId, grant })",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/members/route.ts",
        label: "scoped EVO members route",
        needles: &[
            "requireInternalTenantScope(request)",
            "scope.gymIds.includes(input.gymId)",
            "readEvoCredentialGrant(request)",
            "getEvoRequestContext({",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/members/[idMember]/route.ts",
        label: "scoped EVO member detail route",
        needles: &["requireEvoRouteContext(request, query.gymId, \"sync\")"],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/memberships/route.ts",
        label: "scoped EVO memberships route",
        needles: &["requireEvoRouteContext(request, input.gymId, \"sync\")"],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/receivables/route.ts",
        label: "scoped EVO receivables route",
        needles: &["requireEvoRouteContext(request, input.gymId, \"sync\")"],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/sync-member/route.ts",
        label: "scoped EVO member sync route",
        needles: &[
            "requireEvoRouteContext(request, input.gymId, \"sync_member\")",
            "where: { id: input.memberId, gymId: input.gymId }",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/sync/route.ts",
        label: "trusted tenant-scoped EVO sync route",
        needles: &[
            "requireInternalTenantScope",
            "gym is outside the trusted tenant scope",
            "readEvoCredentialGrant",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/evo/units/onboard/route.ts",
        label: "trusted metadata-only EVO onboarding route",
        needles: &[
            "requireInternalTenant",
            "credentialAlias",
            "officialEmail",
            "validatedProfile",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/collections/overdue/payment-link/route.ts",
        label: "scoped EVO payment-link revalidation route",
        needles: &[
            "requireInternalTenantScope(request)",
            "readEvoCredentialGrant(request)",
            "selectEvoPaymentLink({",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/pagarme/webhooks/route.ts",
        label: "canonical-owner EVO receivable reconciliation",
        needles: &[
            "issueEvoCredentialGrantContext({",
            "getEvoRequestContext({",
            "operation: \"reconcile_receivable\"",
        ],
    },
    Contract {
        path: "jai-pay/src/app/api/pacto/recipients/prefill/route.ts",
        label: "scoped EVO recipient prefill route",
        needles: &[
            "requireInternalTenantScope(request)",
            "evoScope.gymIds.includes(input.gymId)",
            "readEvoCredentialGrant(request)",
            "getEvoRequestContext({",
        ],
    },
    Contract {
        path: "cartridges/evo/config.py",
        label: "shared-profile request credential guard",
        needles: &[
            "EVO_REQUIRE_REQUEST_CREDENTIALS",
            "if require_request_credentials and not",
            "HTTP_503_SERVICE_UNAVAILABLE",
        ],
    },
    Contract {
        path: "jai-pay/src/lib/payment-link-authority.ts",
        label: "explicit EVO/Pacto/JAI Pay link authority",
        needles: &[
            "erp_native",
            "jaipay_native",
            "erp_then_jaipay",
            "authority.strategy !== \"erp_then_jaipay\"",
            "error instanceof PaymentLinkAuthorityError",
            "FALLBACK_FAILURES.has(error.classification)",
        ],
    },
    Contract {
        path: "jai-pay/src/lib/fitness-erp.ts",
        label: "tenant-scoped canonical fitness snapshot",
        needles: &[
            "getFitnessErpSnapshot(scope: TenantGymScope)",
            "const gyms = await loadFitnessGyms(scope);",
            "where: { id: { in: [...scope.gymIds] } }",
        ],
    },
    Contract {
        path: "example-api/example/integrations/whatsapp/tool.py",
        label: "Rust-controlled WhatsApp egress handoff",
        needles: &["handoff_to_system2", "_send_via_egress"],
    },
];

const SECRET_SET_PATHS: &[&str] = &[
    "deploy/secret-sets/customer_ops_unified.env.example",
    "deploy/secret-sets/collections_platform.env.example",
];
const CUSTOMER_OPS_PROFILE_PATH: &str = "deploy/profiles/customer_ops_unified.env";
const GYM_SCHEMA_PATH: &str = "jai-pay/prisma/schema.prisma";
const EVO_ONBOARDING_PATH: &str = "jai-pay/src/lib/evo-unit-onboarding.ts";
const REQUIRED_ACTIVE_CARTRIDGES: &[&str] = &["evo", "pacto", "jaipay", "revops"];
const EVO_API_ENTRYPOINTS: &[&str] = &[
    "jai-pay/src/app/api/evo/members/route.ts",
    "jai-pay/src/app/api/evo/members/[idMember]/route.ts",
    "jai-pay/src/app/api/evo/memberships/route.ts",
    "jai-pay/src/app/api/evo/receivables/route.ts",
    "jai-pay/src/app/api/evo/sync-member/route.ts",
    "jai-pay/src/app/api/evo/sync/route.ts",
    "jai-pay/src/app/api/evo/units/onboard/route.ts",
    "jai-pay/src/app/api/collections/overdue/payment-link/route.ts",
    "jai-pay/src/app/api/pagarme/webhooks/route.ts",
    "jai-pay/src/app/api/pacto/recipients/prefill/route.ts",
];
const EVO_API_IMPORTS: &[&str] = &["@/lib/evo\"", "@/lib/evo'", "@/lib/evo-"];
const FORBIDDEN_BROWSER_EVO_PREFIX: &str = "NEXT_PUBLIC_EVO_";
const BROWSER_SOURCE_ROOTS: &[&str] = &["example-ops/src", "jai-pay/src"];
const FORBIDDEN_SECRET_SET_EVO: &[&str] = &[
    "EVO_CREDENTIALS_JSON",
    "EVO_CREDENTIALS_FILE",
    "EVO_API_USERNAME",
    "EVO_API_SECRET",
];
const FORBIDDEN_PRODUCTION_EVO: &[(&str, &str)] = &[
    ("jai-pay/src", "EVO_CREDENTIALS_JSON"),
    ("jai-pay/src", "EVO_CREDENTIALS_FILE"),
    (
        "example-api/example/integrations/evo_credential_store.py",
        "versions[].value",
    ),
];
const FORBIDDEN_EVO_ROUTE_FIELDS: &[(&str, &str)] = &[
    (
        "jai-pay/src/app/api/evo/units/onboard/route.ts",
        "username:",
    ),
    ("jai-pay/src/app/api/evo/units/onboard/route.ts", "token:"),
    ("jai-pay/src/app/api/evo/sync/route.ts", "username:"),
    ("jai-pay/src/app/api/evo/sync/route.ts", "token:"),
];
const FORBIDDEN_DIRECT_WHATSAPP_EGRESS: &[&str] = &[
    "graph.facebook.com",
    "WHATSAPP_ACCESS_TOKEN",
    "httpx.post(",
    "requests.post(",
    "urllib.request",
];

fn push_missing(
    warnings: &mut Vec<String>,
    evidence: &mut Vec<EvidenceItem>,
    path: &str,
    label: &str,
    missing: &[&str],
    body: Option<&str>,
) {
    warnings.push(format!(
        "{label} drift in {path}: missing {}",
        missing.join(", ")
    ));
    evidence.push(EvidenceItem {
        kind: "c4gym_evo_missing_contract".to_string(),
        path: path.to_string(),
        line: body.and_then(|source| missing.first().and_then(|needle| find_line(source, needle))),
        detail: format!("{label}; required: {}", missing.join(", ")),
    });
}

fn production_broker_callers(root: &Path) -> Vec<(String, usize)> {
    let source = root.join("jai-pay/src");
    if !source.exists() {
        return Vec::new();
    }
    let mut callers = Vec::new();
    for entry in WalkBuilder::new(source)
        .hidden(false)
        .git_ignore(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file()
            || path.to_string_lossy().contains(".test.")
            || path.ends_with("evo-credential-broker.ts")
        {
            continue;
        }
        let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
            continue;
        };
        if !matches!(extension, "ts" | "tsx") {
            continue;
        }
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        if let Some(line) = find_awaited_function_call_line(&body, "resolveEvoCredential")
            && let Ok(relative) = path.strip_prefix(root)
        {
            callers.push((relative.to_string_lossy().replace('\\', "/"), line));
        }
    }
    callers
}

fn active_cartridges(body: &str) -> Option<Vec<String>> {
    let value = body.lines().find_map(|line| {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') {
            return None;
        }
        trimmed.strip_prefix("EXAMPLE_ACTIVE_CARTRIDGES=")
    })?;
    Some(
        value
            .split('#')
            .next()
            .unwrap_or_default()
            .trim()
            .trim_matches(&['\'', '"'][..])
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect(),
    )
}

fn shared_contact_identity_drift(root: &Path) -> Vec<String> {
    let schema = std::fs::read_to_string(root.join(GYM_SCHEMA_PATH)).unwrap_or_default();
    let onboarding = std::fs::read_to_string(root.join(EVO_ONBOARDING_PATH)).unwrap_or_default();
    let mut warnings = Vec::new();
    let active_schema = prisma_code_only(&schema);
    let gym_model = prisma_model_body(&active_schema, "Gym").unwrap_or_default();
    let email_is_unique = gym_email_is_unique(gym_model);
    if email_is_unique || !gym_has_email_index(gym_model) {
        warnings.push("Gym contact email identity must be non-unique and indexed".to_string());
    }
    let unit = TypeScriptModule::parse(&onboarding);
    if unit
        .as_ref()
        .is_none_or(|source| source.onboarding_uses_email_or_lacks_branch_tuple())
    {
        warnings.push("EVO onboarding contact email identity drift".to_string());
    }
    warnings
}

fn prisma_model_body<'source>(schema: &'source str, model: &str) -> Option<&'source str> {
    let marker = format!("model {model}");
    let start = schema.match_indices(&marker).find_map(|(start, _)| {
        let preceding = schema.as_bytes().get(start.wrapping_sub(1)).copied();
        let following = schema.as_bytes().get(start + marker.len()).copied();
        (preceding.is_none_or(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
            && following.is_some_and(|byte| byte.is_ascii_whitespace() || byte == b'{'))
        .then_some(start)
    })?;
    let open = schema[start + marker.len()..].find('{')? + start + marker.len();
    let mut depth = 0usize;
    for (offset, byte) in schema.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(&schema[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn prisma_code_only(schema: &str) -> String {
    let bytes = schema.as_bytes();
    let mut output = bytes.to_vec();
    let mut index = 0;
    let mut line_comment = false;
    let mut block_comment = false;
    let mut quoted = None;
    while index < bytes.len() {
        if let Some(quote) = quoted {
            if bytes[index] == b'\\' {
                output[index] = b' ';
                if bytes.get(index + 1).is_some_and(|byte| *byte != b'\n') {
                    output[index + 1] = b' ';
                }
                index += 2;
                continue;
            }
            if bytes[index] == quote {
                output[index] = b' ';
                quoted = None;
            } else if bytes[index] != b'\n' {
                output[index] = b' ';
            }
            index += 1;
            continue;
        }
        if line_comment {
            if bytes[index] == b'\n' {
                line_comment = false;
            } else {
                output[index] = b' ';
            }
            index += 1;
            continue;
        }
        if block_comment {
            if bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/') {
                output[index] = b' ';
                output[index + 1] = b' ';
                block_comment = false;
                index += 2;
            } else {
                if bytes[index] != b'\n' {
                    output[index] = b' ';
                }
                index += 1;
            }
            continue;
        }
        match bytes[index] {
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                output[index] = b' ';
                output[index + 1] = b' ';
                line_comment = true;
                index += 2;
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                output[index] = b' ';
                output[index + 1] = b' ';
                block_comment = true;
                index += 2;
            }
            b'\'' | b'"' => {
                output[index] = b' ';
                quoted = Some(bytes[index]);
                index += 1;
            }
            _ => index += 1,
        }
    }
    String::from_utf8(output).expect("comment stripping preserves UTF-8")
}

fn gym_has_email_index(model: &str) -> bool {
    gym_has_email_model_attribute(model, "@@index")
}

fn gym_email_is_unique(model: &str) -> bool {
    gym_email_field_has_unique_attribute(model) || gym_has_email_model_attribute(model, "@@unique")
}

fn gym_email_field_has_unique_attribute(model: &str) -> bool {
    model.lines().any(|line| {
        let mut tokens = line.split_whitespace();
        matches!(tokens.next(), Some("email"))
            && tokens.next().is_some()
            && tokens.any(|token| token == "@unique" || token.starts_with("@unique("))
    })
}

fn gym_has_email_model_attribute(model: &str, attribute: &str) -> bool {
    model.lines().any(|line| {
        let line = line.trim_start();
        line.starts_with(&format!("{attribute}("))
            && line
                .split_once('[')
                .and_then(|(_, tail)| tail.split_once(']'))
                .is_some_and(|(fields, _)| {
                    fields.split(',').any(|field| {
                        field.split_once('(').map_or(field, |(name, _)| name).trim() == "email"
                    })
                })
    })
}

struct TypeScriptModule<'source> {
    source: &'source str,
    tree: Tree,
}

impl<'source> TypeScriptModule<'source> {
    fn parse(source: &'source str) -> Option<Self> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into())
            .ok()?;
        let tree = parser.parse(source, None)?;
        if tree.root_node().has_error() {
            return None;
        }
        Some(Self { source, tree })
    }

    fn onboarding_uses_email_or_lacks_branch_tuple(&self) -> bool {
        let Some(onboarding) = self.named_function("onboardEvoUnit") else {
            return true;
        };
        let Some(body) = onboarding.child_by_field_name("body") else {
            return true;
        };
        let mut saw_branch_tuple = false;
        let mut saw_email_lookup = false;
        self.walk_calls(body, body, &mut saw_branch_tuple, &mut saw_email_lookup);
        saw_email_lookup || !saw_branch_tuple
    }

    fn named_function(&self, function: &str) -> Option<Node<'_>> {
        self.find_named_function(self.tree.root_node(), function)
    }

    fn find_named_function<'a>(&self, node: Node<'a>, function: &str) -> Option<Node<'a>> {
        if node.kind() == "function_declaration"
            && node
                .child_by_field_name("name")
                .and_then(|name| name.utf8_text(self.source.as_bytes()).ok())
                .is_some_and(|name| name == function)
        {
            return Some(node);
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find_map(|child| self.find_named_function(child, function))
    }

    fn walk_calls(
        &self,
        node: Node<'_>,
        onboarding_body: Node<'_>,
        saw_branch_tuple: &mut bool,
        saw_email_lookup: &mut bool,
    ) {
        if node.kind() == "call_expression" && self.is_find_unique(node) {
            *saw_email_lookup |= self.is_email_lookup(node);
            *saw_branch_tuple |= self.is_branch_tuple_lookup(node);
        }
        if matches!(node.kind(), "function_declaration" | "method_definition") {
            return;
        }
        if node.kind() == "arrow_function"
            && !self.arrow_is_executable_callback(node, onboarding_body)
        {
            return;
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            self.walk_calls(child, onboarding_body, saw_branch_tuple, saw_email_lookup);
        }
    }

    fn arrow_is_executable_callback(&self, arrow: Node<'_>, _onboarding_body: Node<'_>) -> bool {
        let Some(parent) = arrow.parent() else {
            return false;
        };
        if parent.kind() == "arguments" {
            return true;
        }
        if parent.kind() != "variable_declarator" {
            return false;
        }
        let Some(name) = parent.child_by_field_name("name") else {
            return false;
        };
        let Ok(name) = name.utf8_text(self.source.as_bytes()) else {
            return false;
        };
        self.lexical_scope(parent).is_some_and(|scope| {
            let mut cursor = scope.walk();
            scope
                .named_children(&mut cursor)
                .any(|child| self.has_direct_identifier_call_in_scope(child, name, arrow, false))
        })
    }

    fn lexical_scope<'a>(&self, node: Node<'a>) -> Option<Node<'a>> {
        let mut parent = Some(node);
        while let Some(current) = parent {
            if matches!(current.kind(), "statement_block" | "program") {
                return Some(current);
            }
            parent = current.parent();
        }
        None
    }

    fn has_direct_identifier_call_in_scope(
        &self,
        node: Node<'_>,
        name: &str,
        binding: Node<'_>,
        shadowed: bool,
    ) -> bool {
        if node.start_byte() == binding.start_byte() && node.end_byte() == binding.end_byte() {
            return false;
        }
        if matches!(
            node.kind(),
            "function_declaration" | "method_definition" | "arrow_function"
        ) {
            return false;
        }
        if !shadowed
            && node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .is_some_and(|function| {
                    function.kind() == "identifier" && self.node_text(function) == name
                })
        {
            return true;
        }
        let shadowed = shadowed || self.scope_shadows_binding(node, name, binding);
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| self.has_direct_identifier_call_in_scope(child, name, binding, shadowed))
    }

    fn scope_shadows_binding(&self, node: Node<'_>, name: &str, binding: Node<'_>) -> bool {
        if !matches!(
            node.kind(),
            "statement_block" | "for_statement" | "catch_clause"
        ) {
            return false;
        }
        let Some(binding_declarator) = binding.parent() else {
            return false;
        };
        if node.kind() == "catch_clause"
            && node
                .child_by_field_name("parameter")
                .is_some_and(|parameter| self.node_text(parameter) == name)
        {
            return true;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| self.scope_declares_name(child, name, binding_declarator))
    }

    fn scope_declares_name(&self, node: Node<'_>, name: &str, binding: Node<'_>) -> bool {
        if node.kind() == "variable_declarator" {
            return !(node.start_byte() == binding.start_byte()
                && node.end_byte() == binding.end_byte())
                && node
                    .child_by_field_name("name")
                    .is_some_and(|candidate| self.node_text(candidate) == name);
        }
        if matches!(node.kind(), "function_declaration" | "method_definition") {
            return node
                .child_by_field_name("name")
                .is_some_and(|candidate| self.node_text(candidate) == name);
        }
        if node.kind() == "arrow_function" {
            return false;
        }
        if matches!(
            node.kind(),
            "statement_block" | "for_statement" | "catch_clause"
        ) {
            return false;
        }
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .any(|child| self.scope_declares_name(child, name, binding))
    }

    fn is_find_unique(&self, call: Node<'_>) -> bool {
        call.child_by_field_name("function")
            .and_then(|function| function.child_by_field_name("property"))
            .and_then(|property| property.utf8_text(self.source.as_bytes()).ok())
            .is_some_and(|property| property == "findUnique")
    }

    fn is_email_lookup(&self, call: Node<'_>) -> bool {
        self.where_object(call).is_some_and(|where_object| {
            self.object_property(where_object, "email")
                .is_some_and(|value| self.node_text(value) == "input.officialEmail")
        })
    }

    fn is_branch_tuple_lookup(&self, call: Node<'_>) -> bool {
        let Some(function) = call.child_by_field_name("function") else {
            return false;
        };
        let Some(object) = function.child_by_field_name("object") else {
            return false;
        };
        if self.node_text(object) != "tx.gymProviderAccount" {
            return false;
        }
        let Some(where_object) = self.where_object(call) else {
            return false;
        };
        let Some(tuple) = self.object_property(where_object, "provider_externalType_externalId")
        else {
            return false;
        };
        self.object_property(tuple, "provider")
            .is_some_and(|value| self.node_text(value) == "\"evo\"")
            && self
                .object_property(tuple, "externalType")
                .is_some_and(|value| self.node_text(value) == "\"branch\"")
            && self
                .object_property(tuple, "externalId")
                .is_some_and(|value| {
                    matches!(
                        self.node_text(value),
                        "profile.idBranch" | "String(profile.idBranch)"
                    )
                })
    }

    fn where_object<'a>(&self, call: Node<'a>) -> Option<Node<'a>> {
        let arguments = call.child_by_field_name("arguments")?;
        let mut cursor = arguments.walk();
        arguments
            .named_children(&mut cursor)
            .find(|child| child.kind() == "object")
            .and_then(|object| self.object_property(object, "where"))
    }

    fn object_property<'a>(&self, object: Node<'a>, name: &str) -> Option<Node<'a>> {
        if object.kind() != "object" {
            return None;
        }
        let mut cursor = object.walk();
        object.named_children(&mut cursor).find_map(|pair| {
            if pair.kind() != "pair" {
                return None;
            }
            let key = pair.child_by_field_name("key")?;
            (self.node_text(key).trim_matches(&['\'', '"'][..]) == name)
                .then(|| pair.child_by_field_name("value"))
                .flatten()
        })
    }

    fn node_text(&self, node: Node<'_>) -> &str {
        node.utf8_text(self.source.as_bytes())
            .unwrap_or_default()
            .trim()
    }
}

fn forbidden_browser_evo(root: &Path) -> Vec<(String, usize)> {
    let mut hits = Vec::new();
    for source_root in BROWSER_SOURCE_ROOTS {
        let source = root.join(source_root);
        if !source.exists() {
            continue;
        }
        for entry in WalkBuilder::new(source)
            .hidden(false)
            .git_ignore(true)
            .build()
            .flatten()
        {
            let path = entry.path();
            if !path.is_file() || path.to_string_lossy().contains(".test.") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(path) else {
                continue;
            };
            if let Some(line) = find_line(&body, FORBIDDEN_BROWSER_EVO_PREFIX)
                && let Ok(relative) = path.strip_prefix(root)
            {
                hits.push((relative.to_string_lossy().replace('\\', "/"), line));
            }
        }
    }
    hits
}

fn assignment_line(body: &str, name: &str) -> Option<usize> {
    body.lines().enumerate().find_map(|(index, line)| {
        let trimmed = line.trim_start();
        (trimmed.starts_with(&format!("{name}="))
            || trimmed.starts_with(&format!("export {name}=")))
        .then_some(index + 1)
    })
}

fn forbidden_production_evo(root: &Path) -> Vec<(String, usize, String)> {
    let mut hits = Vec::new();
    for (relative_root, needle) in FORBIDDEN_PRODUCTION_EVO {
        let source = root.join(relative_root);
        if source.is_file() {
            if let Ok(body) = std::fs::read_to_string(&source)
                && let Some(line) = find_line(&body, needle)
            {
                hits.push((relative_root.to_string(), line, needle.to_string()));
            }
            continue;
        }
        if !source.exists() {
            continue;
        }
        for entry in WalkBuilder::new(source)
            .hidden(false)
            .git_ignore(true)
            .build()
            .flatten()
        {
            let path = entry.path();
            if !path.is_file() || path.to_string_lossy().contains(".test.") {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(path) else {
                continue;
            };
            if let Some(line) = find_line(&body, needle)
                && let Ok(relative) = path.strip_prefix(root)
            {
                hits.push((
                    relative.to_string_lossy().replace('\\', "/"),
                    line,
                    needle.to_string(),
                ));
            }
        }
    }
    hits
}

fn unenumerated_evo_api_entrypoints(root: &Path) -> Vec<(String, usize)> {
    let source = root.join("jai-pay/src/app/api");
    if !source.exists() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    for entry in WalkBuilder::new(source)
        .hidden(false)
        .git_ignore(true)
        .build()
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() || path.to_string_lossy().contains(".test.") {
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let Ok(body) = std::fs::read_to_string(path) else {
            continue;
        };
        let is_evo_namespace_route =
            relative.starts_with("jai-pay/src/app/api/evo/") && relative.ends_with("/route.ts");
        let import_line = EVO_API_IMPORTS
            .iter()
            .find_map(|needle| find_line(&body, needle));
        if (is_evo_namespace_route || import_line.is_some())
            && !EVO_API_ENTRYPOINTS.contains(&relative.as_str())
        {
            hits.push((relative, import_line.unwrap_or(1)));
        }
    }
    hits
}

pub fn doctor_c4gym_evo_integration(root: &Path) -> QueryEnvelope {
    let started = Instant::now();
    let mut warnings = Vec::new();
    let mut evidence = Vec::new();
    let mut checks_passed = 0usize;
    let checks_total = CONTRACTS.len() + SECRET_SET_PATHS.len() + 8;

    for contract in CONTRACTS {
        let mut io_warnings = Vec::new();
        match read_text(&root.join(contract.path), &mut io_warnings) {
            Some(body) => {
                let missing = contract
                    .needles
                    .iter()
                    .copied()
                    .filter(|needle| !body.contains(needle))
                    .collect::<Vec<_>>();
                if missing.is_empty() {
                    checks_passed += 1;
                } else {
                    push_missing(
                        &mut warnings,
                        &mut evidence,
                        contract.path,
                        contract.label,
                        &missing,
                        Some(&body),
                    );
                }
            }
            None => {
                warnings.push(format!(
                    "{} missing or unreadable: {}",
                    contract.label, contract.path
                ));
                warnings.extend(io_warnings);
                evidence.push(EvidenceItem {
                    kind: "c4gym_evo_missing_file".to_string(),
                    path: contract.path.to_string(),
                    line: None,
                    detail: contract.label.to_string(),
                });
            }
        }
    }

    let mut io_warnings = Vec::new();
    match read_text(&root.join(CUSTOMER_OPS_PROFILE_PATH), &mut io_warnings) {
        Some(body) => {
            let active = active_cartridges(&body).unwrap_or_default();
            let missing = REQUIRED_ACTIVE_CARTRIDGES
                .iter()
                .copied()
                .filter(|required| !active.iter().any(|name| name == required))
                .collect::<Vec<_>>();
            if missing.is_empty() {
                checks_passed += 1;
            } else {
                push_missing(
                    &mut warnings,
                    &mut evidence,
                    CUSTOMER_OPS_PROFILE_PATH,
                    "active EVO and JAI Pay cartridges",
                    &missing,
                    Some(&body),
                );
            }
        }
        None => {
            warnings.push(format!(
                "active EVO and JAI Pay cartridges missing or unreadable: {CUSTOMER_OPS_PROFILE_PATH}"
            ));
            warnings.extend(io_warnings);
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_missing_active_cartridges".to_string(),
                path: CUSTOMER_OPS_PROFILE_PATH.to_string(),
                line: None,
                detail: format!("required: {}", REQUIRED_ACTIVE_CARTRIDGES.join(", ")),
            });
        }
    }

    for path in SECRET_SET_PATHS {
        let mut io_warnings = Vec::new();
        match read_text(&root.join(path), &mut io_warnings) {
            Some(body) => {
                let required = [
                    "EVO_REQUIRE_REQUEST_CREDENTIALS=true",
                    "EVO_BROKER_JAIPAY_HMAC_KEY=",
                    "EVO_BROKER_JAIPAY_HMAC_KEY_ID=",
                    "EVO_CREDENTIAL_KMS_KEY_NAME=",
                ];
                let missing = required
                    .iter()
                    .copied()
                    .filter(|needle| !body.contains(needle))
                    .collect::<Vec<_>>();
                let forbidden = FORBIDDEN_SECRET_SET_EVO
                    .iter()
                    .filter_map(|name| assignment_line(&body, name).map(|line| (*name, line)))
                    .collect::<Vec<_>>();
                if missing.is_empty() && forbidden.is_empty() {
                    checks_passed += 1;
                } else {
                    if !missing.is_empty() {
                        push_missing(
                            &mut warnings,
                            &mut evidence,
                            path,
                            "EVO deployment secret contract",
                            &missing,
                            Some(&body),
                        );
                    }
                    for (name, line) in forbidden {
                        warnings.push(format!(
                            "forbidden EVO global credential assignment at {path}:{line}: {name}"
                        ));
                        evidence.push(EvidenceItem {
                            kind: "c4gym_evo_forbidden_secret_set_credential".to_string(),
                            path: path.to_string(),
                            line: Some(line),
                            detail: format!("forbidden production secret-set assignment: {name}"),
                        });
                    }
                }
            }
            None => {
                warnings.push(format!(
                    "EVO deployment secret contract missing or unreadable: {path}"
                ));
                warnings.extend(io_warnings);
            }
        }
    }

    let contact_identity_drift = shared_contact_identity_drift(root);
    if contact_identity_drift.is_empty() {
        checks_passed += 1;
    } else {
        for warning in contact_identity_drift {
            warnings.push(warning.clone());
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_contact_identity_drift".to_string(),
                path: format!("{GYM_SCHEMA_PATH}, {EVO_ONBOARDING_PATH}"),
                line: None,
                detail: warning,
            });
        }
    }

    let callers = production_broker_callers(root);
    if callers.is_empty() {
        warnings.push("EVO credential broker has no production caller; tests alone cannot prove credential use wiring".to_string());
        evidence.push(EvidenceItem {
            kind: "c4gym_evo_test_only_broker_caller".to_string(),
            path: "jai-pay/src".to_string(),
            line: None,
            detail: "expected a non-test resolveEvoCredential caller".to_string(),
        });
    } else {
        checks_passed += 1;
    }

    let browser_hits = forbidden_browser_evo(root);
    if browser_hits.is_empty() {
        checks_passed += 1;
    } else {
        for (path, line) in browser_hits {
            warnings.push(format!(
                "browser-exposed EVO environment contract at {path}:{line}"
            ));
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_browser_secret".to_string(),
                path,
                line: Some(line),
                detail: format!("forbidden {FORBIDDEN_BROWSER_EVO_PREFIX} prefix"),
            });
        }
    }

    let forbidden_hits = forbidden_production_evo(root);
    if forbidden_hits.is_empty() {
        checks_passed += 1;
    } else {
        for (path, line, needle) in forbidden_hits {
            warnings.push(format!(
                "forbidden EVO credential fallback at {path}:{line}: {needle}"
            ));
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_forbidden_fallback".to_string(),
                path,
                line: Some(line),
                detail: format!("forbidden production credential path: {needle}"),
            });
        }
    }

    let unenumerated_entrypoints = unenumerated_evo_api_entrypoints(root);
    if unenumerated_entrypoints.is_empty() {
        checks_passed += 1;
    } else {
        for (path, line) in unenumerated_entrypoints {
            warnings.push(format!(
                "EVO API entrypoint is outside the tenant/gym/grant contract registry at {path}:{line}"
            ));
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_unenumerated_api_entrypoint".to_string(),
                path,
                line: Some(line),
                detail: "enumerate the route and require its exact trusted scope or canonical-owner grant path"
                    .to_string(),
            });
        }
    }

    let mut raw_route_fields = Vec::new();
    for (path, field) in FORBIDDEN_EVO_ROUTE_FIELDS {
        if let Ok(body) = std::fs::read_to_string(root.join(path))
            && let Some(line) = find_line(&body, field)
        {
            raw_route_fields.push((path.to_string(), line, field.to_string()));
        }
    }
    if raw_route_fields.is_empty() {
        checks_passed += 1;
    } else {
        for (path, line, field) in raw_route_fields {
            warnings.push(format!(
                "raw EVO credential field in browser-reachable route at {path}:{line}: {field}"
            ));
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_raw_route_credential".to_string(),
                path,
                line: Some(line),
                detail:
                    "onboarding and sync accept alias/grant only, never raw provider credentials"
                        .to_string(),
            });
        }
    }

    let egress_path = "example-api/example/integrations/whatsapp/tool.py";
    let mut direct_egress = Vec::new();
    if let Ok(body) = std::fs::read_to_string(root.join(egress_path)) {
        for needle in FORBIDDEN_DIRECT_WHATSAPP_EGRESS {
            if let Some(line) = find_line(&body, needle) {
                direct_egress.push((*needle, line));
            }
        }
    }
    if direct_egress.is_empty() {
        checks_passed += 1;
    } else {
        for (needle, line) in direct_egress {
            warnings.push(format!(
                "direct WhatsApp provider egress bypass at {egress_path}:{line}: {needle}"
            ));
            evidence.push(EvidenceItem {
                kind: "c4gym_evo_direct_whatsapp_egress".to_string(),
                path: egress_path.to_string(),
                line: Some(line),
                detail: "outbound WhatsApp must terminate in the Rust egress plane".to_string(),
            });
        }
    }

    QueryEnvelope {
        schema_version: crate::model::SCHEMA_VERSION.to_string(),
        query_id: query_id("doctor_c4gym_evo_integration"),
        kind: "doctor".to_string(),
        summary: if warnings.is_empty() {
            "C4 GYM EVO integration contracts are wired through tenant scope, broker, explicit link authority, and Rust egress".to_string()
        } else {
            format!(
                "C4 GYM EVO integration drift: {} warning(s)",
                warnings.len()
            )
        },
        confidence: if warnings.is_empty() { 0.95 } else { 0.62 },
        entities: vec![json!({
            "doctor": "c4gym-evo-integration", "checks_passed": checks_passed,
            "checks_total": checks_total, "tenant": "c4gym", "source_of_truth": "evo",
            "credential_contract": "tenant and gym scoped KMS broker with one-time grant and JAI Pay HMAC",
            "outbound_contract": "WhatsApp delivery remains in the Rust egress plane",
        })],
        evidence,
        warnings,
        meta: Some(json!({"integration": "c4gym-evo-jaipay-revops"})),
        timing_ms: started.elapsed().as_millis(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn temp_root(name: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("leio_c4gym_evo_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, body: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, body).unwrap();
    }

    fn write_valid(root: &Path) {
        for contract in CONTRACTS {
            let mut body = contract.needles.join("\n");
            if contract.path == "jai-pay/src/lib/evo.ts" {
                body.push_str(
                    "\nconst credential = await resolveEvoCredential({ expectedBranchId: account.branchId });",
                );
            }
            write(root, contract.path, &body);
        }
        write(
            root,
            CUSTOMER_OPS_PROFILE_PATH,
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant,evo,pacto,jaipay,revops",
        );
        let secrets = [
            "EVO_REQUIRE_REQUEST_CREDENTIALS=true",
            "EVO_BROKER_JAIPAY_HMAC_KEY=",
            "EVO_BROKER_JAIPAY_HMAC_KEY_ID=jai-pay-v1",
            "EVO_CREDENTIAL_KMS_KEY_NAME=",
        ]
        .join("\n");
        for path in SECRET_SET_PATHS {
            write(root, path, &secrets);
        }
        write(
            root,
            GYM_SCHEMA_PATH,
            "model Gym {\n  id String @id\n  email String\n\n  @@index([email])\n}",
        );
        write(
            root,
            EVO_ONBOARDING_PATH,
            "export async function onboardEvoUnit() {\n  const run = async () => prisma.$transaction(async (tx) => {\n    return tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } });\n  });\n  return await run();\n}",
        );
    }

    #[test]
    fn accepts_complete_integration_contract() {
        let root = temp_root("valid");
        write_valid(&root);
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(envelope.warnings.is_empty(), "{:?}", envelope.warnings);
    }

    #[test]
    fn accepts_nested_transaction_callback_branch_tuple() {
        let root = temp_root("nested_branch_tuple");
        write_valid(&root);
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn flags_unique_gym_contact_email() {
        let root = temp_root("unique_gym_contact");
        write_valid(&root);
        write(
            &root,
            "jai-pay/prisma/schema.prisma",
            "model Gym {\n  id String @id\n  email String @unique\n\n  @@index([email])\n}",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["Gym contact email identity must be non-unique and indexed"]
        );
    }

    #[test]
    fn flags_model_level_unique_gym_contact_email() {
        let root = temp_root("unique_gym_contact_model");
        write_valid(&root);
        write(
            &root,
            GYM_SCHEMA_PATH,
            "model Gym {\n  id String @id\n  email String\n\n  @@index([email])\n  @@unique([email])\n}",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["Gym contact email identity must be non-unique and indexed"]
        );
    }

    #[test]
    fn flags_gym_contact_email_without_an_index() {
        let root = temp_root("missing_gym_contact_index");
        write_valid(&root);
        write(
            &root,
            GYM_SCHEMA_PATH,
            "model Gym {\n  id String @id\n  email String\n}",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["Gym contact email identity must be non-unique and indexed"]
        );
    }

    #[test]
    fn accepts_named_gym_contact_email_index_and_ignores_other_model_uniqueness() {
        let root = temp_root("named_gym_contact_index");
        write_valid(&root);
        write(
            &root,
            GYM_SCHEMA_PATH,
            "model Gym {\n  id String @id\n  email String\n\n  @@index([email], map: \"Gym_email_idx\")\n}\n\nmodel Other {\n  id String @id\n  email String @unique\n}",
        );
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn ignores_unique_text_inside_a_gym_email_default() {
        let root = temp_root("quoted_unique_default");
        write_valid(&root);
        write(
            &root,
            GYM_SCHEMA_PATH,
            "model Gym {\n  id String @id\n  email String @default(\"literal @unique text\")\n\n  @@index([email])\n}",
        );
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn accepts_gym_email_index_field_and_model_options() {
        let root = temp_root("optioned_email_index");
        write_valid(&root);
        write(
            &root,
            GYM_SCHEMA_PATH,
            "model Gym {\n  id String @id\n  email String\n\n  @@index([email(sort: Desc)], map: \"Gym_email_idx\", type: BTree)\n}",
        );
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn flags_email_first_evo_unit_lookup() {
        let root = temp_root("email_first_onboarding");
        write_valid(&root);
        write(
            &root,
            "jai-pay/src/lib/evo-unit-onboarding.ts",
            "export async function onboardEvoUnit() { return tx.gym.findUnique({ where: { email: input.officialEmail } }); }",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["EVO onboarding contact email identity drift"]
        );
    }

    #[test]
    fn flags_multiline_email_first_evo_unit_lookup() {
        let root = temp_root("multiline_email_first_onboarding");
        write_valid(&root);
        write(
            &root,
            EVO_ONBOARDING_PATH,
            "export async function onboardEvoUnit() {\n  const run = async () => tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } });\n  await run();\n  return tx.gym.findUnique({\n    where: {\n      email: input.officialEmail,\n    },\n  });\n}",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["EVO onboarding contact email identity drift"]
        );
    }

    #[test]
    fn ignores_commented_email_lookup_when_branch_tuple_is_executable() {
        let root = temp_root("commented_email_lookup");
        write_valid(&root);
        write(
            &root,
            EVO_ONBOARDING_PATH,
            "export async function onboardEvoUnit() {\n  /* tx.gym.findUnique({ where: { email: input.officialEmail } }); */\n  return tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } });\n}",
        );
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn flags_dead_branch_tuple_without_an_executable_lookup() {
        let root = temp_root("dead_branch_tuple");
        write_valid(&root);
        write(
            &root,
            EVO_ONBOARDING_PATH,
            "function unused() { return tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } }); }\nexport async function onboardEvoUnit() { return tx.gym.findMany({}); }",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["EVO onboarding contact email identity drift"]
        );
    }

    #[test]
    fn ignores_comment_string_and_member_calls_to_dead_arrow_tuple() {
        for source in [
            "export async function onboardEvoUnit() { const run = async () => tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } }); /* run() */ return tx.gym.findMany({}); }",
            "export async function onboardEvoUnit() { const run = async () => tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } }); const note = \"run()\"; return tx.gym.findMany({}); }",
            "export async function onboardEvoUnit() { const run = async () => tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } }); object.run(); return tx.gym.findMany({}); }",
        ] {
            let root = temp_root("dead_arrow_non_call");
            write_valid(&root);
            write(&root, EVO_ONBOARDING_PATH, source);
            assert_eq!(
                shared_contact_identity_drift(&root),
                vec!["EVO onboarding contact email identity drift"]
            );
        }
    }

    #[test]
    fn ignores_shadowed_inner_call_to_dead_outer_arrow_tuple() {
        let root = temp_root("shadowed_arrow_binding");
        write_valid(&root);
        write(
            &root,
            EVO_ONBOARDING_PATH,
            "export async function onboardEvoUnit() {\n  const run = async () => tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } });\n  {\n    const run = async () => tx.gym.findMany({});\n    await run();\n  }\n  return tx.gym.findMany({});\n}",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["EVO onboarding contact email identity drift"]
        );
    }

    #[test]
    fn accepts_outer_arrow_called_from_nested_for_and_try_blocks() {
        let root = temp_root("nested_outer_arrow_call");
        write_valid(&root);
        write(
            &root,
            EVO_ONBOARDING_PATH,
            "export async function onboardEvoUnit() {\n  const run = async () => tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(profile.idBranch) } } });\n  for (const attempt of [1]) {\n    try {\n      return await run();\n    } catch (error) {\n      continue;\n    }\n  }\n  return null;\n}",
        );
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn flags_email_derived_provider_external_id() {
        let root = temp_root("email_derived_external_id");
        write_valid(&root);
        write(
            &root,
            EVO_ONBOARDING_PATH,
            "export async function onboardEvoUnit() { return tx.gymProviderAccount.findUnique({ where: { provider_externalType_externalId: { provider: \"evo\", externalType: \"branch\", externalId: String(input.officialEmail) } } }); }",
        );
        assert_eq!(
            shared_contact_identity_drift(&root),
            vec!["EVO onboarding contact email identity drift"]
        );
    }

    #[test]
    fn ignores_commented_gym_model_and_email_directives() {
        let root = temp_root("commented_gym_schema");
        write_valid(&root);
        write(
            &root,
            GYM_SCHEMA_PATH,
            "// model Gym { email String @unique }\n/* model Gym { email String @unique } */\nmodel Gym {\n  id String @id\n  email String\n  // @@index([email])\n  @@index([email], map: \"Gym_email_idx\")\n}",
        );
        assert!(shared_contact_identity_drift(&root).is_empty());
    }

    #[test]
    fn flags_a_test_only_evo_broker_caller() {
        let root = temp_root("test_only");
        write(
            &root,
            "jai-pay/src/lib/evo-credential-broker.test.ts",
            "resolveEvoCredential({ gymId: 'test' })",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("production caller"))
        );
    }

    #[test]
    fn flags_definition_only_and_import_only_broker_references() {
        let root = temp_root("non_call_references");
        write(
            &root,
            "jai-pay/src/lib/evo-credential-broker.ts",
            "export async function resolveEvoCredential(input: unknown) { return input; }",
        );
        write(
            &root,
            "jai-pay/src/lib/evo.ts",
            "import { resolveEvoCredential } from './evo-credential-broker';",
        );
        assert!(production_broker_callers(&root).is_empty());
    }

    #[test]
    fn flags_commented_and_local_broker_declarations_as_non_callers() {
        let root = temp_root("commented_declaration_references");
        write(
            &root,
            "jai-pay/src/lib/commented.ts",
            "// const credential = await resolveEvoCredential({});\n/* await resolveEvoCredential({}); */",
        );
        write(
            &root,
            "jai-pay/src/lib/local.ts",
            "async function resolveEvoCredential(input: unknown) { return input; }",
        );
        assert!(production_broker_callers(&root).is_empty());
    }

    #[test]
    fn flags_browser_exposed_evo_environment_contract() {
        let root = temp_root("browser_secret");
        write(
            &root,
            "example-ops/src/lib/client.ts",
            "const credential = process.env.NEXT_PUBLIC_EVO_TOKEN;",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("browser-exposed EVO"))
        );
    }

    #[test]
    fn flags_browser_exposed_evo_environment_contract_in_jai_pay() {
        let root = temp_root("jai_pay_browser_secret");
        write(
            &root,
            "jai-pay/src/app/page.tsx",
            "const credential = process.env.NEXT_PUBLIC_EVO_TOKEN;",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("browser-exposed EVO"))
        );
    }

    #[test]
    fn flags_forbidden_global_credentials_in_production_secret_set() {
        let root = temp_root("secret_set_global_credential");
        write_valid(&root);
        write(
            &root,
            SECRET_SET_PATHS[0],
            "EVO_REQUIRE_REQUEST_CREDENTIALS=true\nEVO_BROKER_JAIPAY_HMAC_KEY=\nEVO_BROKER_JAIPAY_HMAC_KEY_ID=jai-pay-v1\nEVO_CREDENTIAL_KMS_KEY_NAME=\nEVO_API_SECRET=forbidden\n",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("global credential assignment"))
        );
    }

    #[test]
    fn flags_a_legacy_environment_credential_registry() {
        let root = temp_root("legacy_registry");
        write(
            &root,
            "jai-pay/src/lib/evo.ts",
            "const credentials = process.env.EVO_CREDENTIALS_JSON;",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("forbidden EVO credential fallback"))
        );
    }

    #[test]
    fn flags_raw_credentials_in_an_onboarding_route() {
        let root = temp_root("raw_route_field");
        write(
            &root,
            "jai-pay/src/app/api/evo/units/onboard/route.ts",
            "const requestSchema = z.object({ username: z.string() });",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("raw EVO credential field"))
        );
    }

    #[test]
    fn flags_an_evo_route_without_the_tenant_and_grant_gate() {
        let root = temp_root("route_gate");
        write_valid(&root);
        write(
            &root,
            "jai-pay/src/app/api/evo/memberships/route.ts",
            "return listEvoMemberMemberships({ gymId: input.gymId });",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| { warning.contains("scoped EVO memberships route drift") })
        );
    }

    #[test]
    fn flags_a_new_unenumerated_evo_api_entrypoint() {
        let root = temp_root("new_evo_entrypoint");
        write_valid(&root);
        write(
            &root,
            "jai-pay/src/app/api/evo/new-operation/route.ts",
            "export async function GET() { return new Response('unguarded'); }",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("outside the tenant/gym/grant contract registry"))
        );
    }

    #[test]
    fn flags_inactive_cartridge_and_global_fitness_snapshot_regressions() {
        let root = temp_root("cartridge_snapshot");
        write_valid(&root);
        write(
            &root,
            "deploy/profiles/customer_ops_unified.env",
            "EXAMPLE_ACTIVE_CARTRIDGES=assurant",
        );
        write(
            &root,
            "jai-pay/src/lib/fitness-erp.ts",
            "async function loadFitnessGyms(scope: TenantGymScope) { return prisma.gym.findMany(); }\nexport async function getFitnessErpSnapshot(scope: TenantGymScope) { const gyms = await loadFitnessGyms(scope); return gyms; }",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("active EVO and JAI Pay cartridges drift"))
        );
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("tenant-scoped canonical fitness snapshot drift"))
        );
    }

    #[test]
    fn accepts_required_cartridges_in_any_csv_order() {
        let root = temp_root("cartridge_order");
        write_valid(&root);
        write(
            &root,
            "deploy/profiles/customer_ops_unified.env",
            "EXAMPLE_ACTIVE_CARTRIDGES=revops,assurant,jaipay,evo,plusoft,pacto",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            !envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("active EVO and JAI Pay cartridges")),
            "{:?}",
            envelope.warnings
        );
    }

    #[test]
    fn flags_missing_kms_hmac_silent_fallback_and_direct_egress() {
        let root = temp_root("boundary_regressions");
        write_valid(&root);
        write(
            &root,
            SECRET_SET_PATHS[0],
            "EVO_REQUIRE_REQUEST_CREDENTIALS=true",
        );
        write(
            &root,
            "jai-pay/src/lib/payment-link-authority.ts",
            "erp_native\njaipay_native\nerp_then_jaipay\nauthority.strategy !== \"erp_then_jaipay\"",
        );
        write(
            &root,
            "example-api/example/integrations/whatsapp/tool.py",
            "handoff_to_system2\n_send_via_egress\nhttpx.post('https://graph.facebook.com')",
        );
        let envelope = doctor_c4gym_evo_integration(&root);
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("EVO deployment secret contract"))
        );
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("explicit EVO/Pacto/JAI Pay link authority drift"))
        );
        assert!(
            envelope
                .warnings
                .iter()
                .any(|warning| warning.contains("direct WhatsApp provider egress bypass"))
        );
    }
}
