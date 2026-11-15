//! A plugin: one provider's HTTP API as a YAML file, apis/<service>.yaml. The service block (A5),
//! routes with their steps (A3), named answers and data tables (A4). The editor, the API and agents
//! all change the same file.

use std::collections::BTreeMap;

use stuntman_core::{ApiError, Ctx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::expr;

fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct OAuth {
    /// secret://name or a literal
    #[serde(default)]
    pub client_id: String,
    #[serde(default)]
    pub client_secret: String,
    #[serde(default = "yes")]
    pub any_client: bool,
    /// 60m, 10m, 1m
    #[serde(default = "sixty")]
    pub token_ttl: String,
}
fn yes() -> bool {
    true
}
fn sixty() -> String {
    "60m".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Basic {
    /// user → secret://password
    #[serde(default)]
    pub users: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub any_user: bool,
    /// Answer 401 with WWW-Authenticate before the client sends credentials.
    #[serde(default, skip_serializing_if = "is_false")]
    pub challenge: bool,
}

/// Session-cookie portals: a login route issues the cookie; routes with receive.auth: session
/// need it; expire_tokens ends every session ("Session expired · log in again").
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Session {
    #[serde(default = "jsession")]
    pub cookie: String,
    #[serde(default = "thirty")]
    pub ttl: String,
    /// user → secret://password
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub users: BTreeMap<String, String>,
    #[serde(default = "yes")]
    pub any_user: bool,
}
fn jsession() -> String {
    "JSESSIONID".into()
}
fn thirty() -> String {
    "30m".into()
}

/// The provider asks for the app's certificate (mTLS) on the HTTPS port: none, or one not signed
/// by the Stuntman Test CA, fails the TLS handshake.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct ClientCertificate {
    #[serde(default)]
    pub required: bool,
    /// ICP-Brasil style: the certificate must carry a CNPJ (OID 2.16.76.1.3.3), as SEFAZ asks.
    #[serde(default, skip_serializing_if = "is_false")]
    pub icp: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Auth {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth2_client_credentials: Option<OAuth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basic: Option<Basic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<Session>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_certificate: Option<ClientCertificate>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Billing {
    #[serde(default)]
    pub enabled: bool,
    /// Price per billable call, only for the counter (nothing is charged).
    #[serde(default)]
    pub per_call: f64,
    /// "100/day": a strip on Calls above it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub warn_above: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Limits {
    /// "300ms..600ms" or "50ms"
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latency: String,
    /// "10/s"
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub rate: String,
    /// Share of calls answered 503, 0 to 1 (circuit breaker tests).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fail_share: f64,
}
fn is_zero(f: &f64) -> bool {
    *f == 0.0
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Service {
    pub name: String,
    #[serde(default)]
    pub title: String,
    /// official | community | yours
    #[serde(default = "yours")]
    pub origin: String,
    /// "BR", or empty for global
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub region: String,
    /// What the provider calls itself (the real base URL), shown and used to recognise its host.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub base_url: String,
    /// The real host name Stuntman also answers for (with the Stuntman Test CA).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub real_host: String,
    #[serde(default)]
    pub auth: Auth,
    #[serde(default)]
    pub billing: Billing,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// Browsers call it: preflights answered 204 and every answer has Access-Control-Allow-Origin: *.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cors: bool,
    /// Routes match whatever the case of the path ("Api/v2/PadesSignatures" and "api/v2/padessignatures").
    #[serde(default, skip_serializing_if = "is_false")]
    pub ignore_case: bool,
    /// How the send steps of this service sign their webhooks (A10c).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhooks: Option<Webhooks>,
    /// A request repeated with the same key gets the first answer again (A10c).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency: Option<Idempotency>,
}

/// Signing of the webhooks a service sends: hex (HMAC-SHA256 of the body, hex) | hex_ts (of
/// "{timestamp}.{body}", with X-Webhook-Timestamp) | stripe (t=…,v1=…) | svix (svix-id,
/// svix-timestamp, svix-signature v1,base64) | github (sha256=hex) | cloudevents (ce-* headers).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Webhooks {
    #[serde(default = "hex_sign")]
    pub sign: String,
    /// The signature header (each format has its own default).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    /// secret://name or a literal; svix secrets are "whsec_<base64>".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
    /// Seconds the app should accept; old_timestamp goes past it.
    #[serde(default = "s300")]
    pub tolerance: u32,
    /// On purpose, on every send while set: wrong_signature | old_timestamp | no_signature | twice | out_of_order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provoke: Vec<String>,
}
fn hex_sign() -> String {
    "hex".into()
}
fn s300() -> u32 {
    300
}
pub const SIGNS: [&str; 6] = ["hex", "hex_ts", "stripe", "svix", "github", "cloudevents"];
pub const SIGN_PROVOKE: [&str; 5] = ["wrong_signature", "old_timestamp", "no_signature", "twice", "out_of_order"];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Idempotency {
    #[serde(default = "idem_header")]
    pub header: String,
    /// How long a key is kept: 24h.
    #[serde(default = "h24")]
    pub ttl: String,
}
fn idem_header() -> String {
    "Idempotency-Key".into()
}
fn h24() -> String {
    "24h".into()
}

/// The service is also an MCP server (A10b): JSON-RPC on `path` (Streamable HTTP); its tools are
/// the routes that have `tool:`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Mcp {
    #[serde(default = "mcp_path")]
    pub path: String,
    /// serverInfo.name (the service name when empty)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    #[serde(default = "v100")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub instructions: String,
    /// "" (open) | bearer (a token from this service's token route; else 401 with WWW-Authenticate)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// Answer as text/event-stream instead of application/json.
    #[serde(default, skip_serializing_if = "is_false")]
    pub sse: bool,
}
fn mcp_path() -> String {
    "/mcp".into()
}
fn v100() -> String {
    "1.0.0".into()
}

/// One part of a streamed answer: sent `after` the previous one.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Part {
    /// "400ms", "1.2s"; empty: at once
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub after: String,
    /// SSE event name (empty: none)
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event: String,
    /// A template, like a body.
    #[serde(default)]
    pub data: String,
}
fn yours() -> String {
    "yours".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Receive {
    /// "" | none | bearer | basic | session. bearer: a token from this service's token route;
    /// session: the cookie the login route issued.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub auth: String,
    /// Fields that must be in the body (dotted paths); a missing one is answered 400 (or the
    /// answer named by `missing`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub body: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub missing: String,
    /// The body is checked against this schema (a key of spec.schemas); a body that breaks it gets
    /// the answer named by `invalid`, or 400 naming the field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub schema: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invalid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Keep {
    /// The state collection, e.g. validations.
    pub state: String,
    /// The key expression, e.g. body.cpf.
    pub key: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Decide {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub when: Option<String>,
    pub answer: String,
    /// After this answer the service's scenario becomes this (WireMock's newScenarioState).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scenario: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Respond {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latency: String,
}

/// "Answer processing 3 times, then done": counts calls per key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Wait {
    pub key: String,
    pub answer: String,
    pub times: u32,
    pub then: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Send {
    /// URL template (literal or with {{ }}), e.g. {{ body.callback_url }}.
    pub url: String,
    #[serde(default = "post")]
    pub method: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub delay: String,
    /// secret://name: the body is signed with HMAC-SHA256 hex into `header`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub secret: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub header: String,
    /// A SOAP callback: the body is the payload, sent inside an envelope with this SOAPAction.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operation: String,
    /// Overrides service.webhooks.sign for this send (hex | hex_ts | stripe | svix | github | cloudevents).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sign: String,
    /// The event type (CloudEvents ce-type; shown on the call).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub event: String,
    /// More headers, templates: {token: "{{ data.tokens[0] }}"}. Names are templates too (empty = left
    /// out); the name "*" takes "Name: value" lines, one header each.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Sent only when this is true: "body.status == 'paid'".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub when: String,
}
fn post() -> String {
    "POST".into()
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Route {
    /// "POST /datavalid/v5/pessoa-fisica/validacao", {name} segments are path params. Empty for a
    /// SOAP operation.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub route: String,
    /// A SOAP operation (under `operations:`): its name in the WSDL. On a route made from an
    /// OpenAPI spec: its operationId.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub operation: String,
    /// The SOAPAction that picks this operation (from the WSDL binding).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub action: String,
    /// The request and response elements (keys of soap.types).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub input: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output: String,
    #[serde(default, skip_serializing_if = "is_false")]
    pub billable: bool,
    /// The OAuth2 token endpoint of this service (client credentials).
    #[serde(default, skip_serializing_if = "is_false")]
    pub token: bool,
    /// The session portal's login: user and password in, the session cookie out.
    #[serde(default, skip_serializing_if = "is_false")]
    pub login: bool,
    /// An OAuth token endpoint for people: authorization_code (the code a page or a push approval
    /// gave, PKCE S256 checked) or password (CPF + OTP). The token carries the person: auth.person.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub grant: String,
    /// A push approval the app polls: the expression giving its code (query.code); read as `step`
    /// (status pending | approved | rejected | expired, grant, person).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub step: String,
    #[serde(default)]
    pub receive: Receive,
    /// Named values computed before deciding: person: lookup('people', 'cpf', body.cpf).
    #[serde(default, rename = "let", skip_serializing_if = "BTreeMap::is_empty")]
    pub lets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep: Option<Keep>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait: Option<Wait>,
    #[serde(default)]
    pub decide: Vec<Decide>,
    #[serde(default)]
    pub respond: Respond,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub send: Vec<Send>,
    /// A generic failure on this route only: reset | empty | timeout | 401 | 403 | 500 | 503 | html.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fail: String,
    /// Response fields and their types (number, string, bool, object, array): answers are checked.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub schema: BTreeMap<String, String>,
    /// GraphQL: the operation name, or the first field of the query, this route answers on its
    /// path (POST /graphql). The scope gets `variables` and `operation`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub graphql: String,
    /// When no rule matches (the answer is not_matched), the next route that matches the path is
    /// tried, as WireMock does across mappings (imported routes).
    #[serde(default, skip_serializing_if = "is_false")]
    pub fallthrough: bool,
    /// MCP: this route is a tool of the service's MCP server; `body` is the tool's arguments.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// The tool's inputSchema (JSON Schema); arguments that break it get isError.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<Value>,
    /// A step Stuntman runs in code instead of let/decide (restpki.start…): it picks one of the
    /// plugin's answers and gives it {{ result }} to render and {{ call }} for the sentence.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub native: String,
}

impl Route {
    pub fn method_path(&self) -> (String, String) {
        if self.route.is_empty() && !self.tool.is_empty() {
            return ("MCP".into(), self.tool.clone());
        }
        if self.route.is_empty() && !self.operation.is_empty() {
            return ("SOAP".into(), self.operation.clone());
        }
        let mut p = self.route.trim().splitn(2, ' ');
        let m = p.next().unwrap_or("GET").to_uppercase();
        let path = p.next().unwrap_or("/").trim().to_string();
        (m, path)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Answer {
    pub name: String,
    #[serde(default = "s200")]
    pub status: u16,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// A template; JSON unless headers say otherwise.
    #[serde(default)]
    pub body: String,
    /// Sent exactly as typed, not checked against the schema (malformed on purpose).
    #[serde(default, skip_serializing_if = "is_false")]
    pub raw: bool,
    /// The sentence on Calls, a template: "Validated {{ body.cpf | mask }} with a selfie".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sentence: String,
    /// What this answer means, shown under its name.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub note: String,
    /// A file kept next to the plugin (apis/<service>.files/<file>), sent as is instead of `body`:
    /// a PDF, an image, anything binary.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub file: String,
    /// A SOAP fault instead of a response; the body (if any) is its detail.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fault: Option<Fault>,
    /// Sent in parts instead of `body` (A10): server-sent events, NDJSON lines or plain chunks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stream: Vec<Part>,
    /// sse (default) | ndjson | chunks
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stream_format: String,
    /// handlebars: the body is a WireMock response template (request.*, now, randomValue, xPath…).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub template: String,
    /// Wait before answering ("800ms"), instead of the route's latency.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub latency: String,
    /// MCP: a JSON-RPC error (-32602…) instead of a tool result; the body is its message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rpc_error: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Fault {
    /// Sender | Receiver (SOAP 1.2); Client | Server are the same in 1.1.
    #[serde(default = "sender")]
    pub code: String,
    /// A template: "Coverage exceeded: {{ body.cargoValue }}".
    pub reason: String,
}
fn sender() -> String {
    "Sender".into()
}

/// A SOAP service: made from the provider's WSDL. Operations take the same steps as routes.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Soap {
    /// "1.1", "1.2", or both (the WSDL has both bindings: each call is answered in its own version)
    #[serde(default = "v12")]
    pub version: String,
    /// The path the provider answers on, e.g. /PolicyService.
    pub endpoint: String,
    /// Where the WSDL came from (a URL): reimport_wsdl reads it again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// The WSDL file kept next to the plugin (apis/<service>.soap/<file>), served back on ?wsdl.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wsdl: String,
    /// Bodies are checked against `types`; a wrong one gets the provider's validation fault.
    #[serde(default = "yes")]
    pub validate: bool,
    /// "" (none) | any (a UsernameToken, any password) | username_token (checked against users).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub security: String,
    /// user → secret://password, for security: username_token.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub users: BTreeMap<String, String>,
    /// Element → its fields: {shipmentNumber: string, cargoValue: decimal, notes?: string, items*: Item}.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub types: BTreeMap<String, BTreeMap<String, String>>,
    /// Element → its target namespace.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub namespace: String,
    /// XML signatures (A18): check the request's ds:Signature, sign the answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<SoapSignature>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct SoapSignature {
    /// Each request must carry a valid ds:Signature (digest and value checked).
    #[serde(default, skip_serializing_if = "is_false")]
    pub check: bool,
    /// The answer named here is given when the signature is missing or broken (else a fault).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invalid: String,
    /// Sign the answers: the element (local name, with an Id attribute) to sign, e.g. infProt.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub sign: String,
    /// RSA-SHA256 and SHA-256 digests (default: RSA-SHA1, as NF-e and CT-e).
    #[serde(default, skip_serializing_if = "is_false")]
    pub sha256: bool,
}
/// A service made from an OpenAPI 3.x or Swagger 2.0 spec: the spec kept next to the plugin and
/// the schemas the routes check bodies against.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Spec {
    /// The spec file kept in apis/<service>.spec/, served back on /<file>.
    pub openapi: String,
    /// Where it came from (a URL): reimport_spec reads it again.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// "3.1.0", "2.0"…
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    /// The spec's own version (info.version).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub api_version: String,
    #[serde(default = "yes")]
    pub validate: bool,
    /// Name → JSON Schema, $refs resolved: what receive.schema names.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub schemas: BTreeMap<String, Value>,
}

/// A recording: calls go to the real provider (its sandbox) and back; each pair is kept, with
/// secrets out, to make the plugin from.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Record {
    /// https://sandbox.provider.example
    pub upstream: String,
    /// every (each call goes to the provider) | unknown (routes answer; the rest go)
    #[serde(default = "every")]
    pub mode: String,
    /// ms; calls since then are the recording
    #[serde(default)]
    pub started: i64,
    /// ms; it stops by itself then (0: never)
    #[serde(default, skip_serializing_if = "is_zero_i")]
    pub until: i64,
    /// it stops by itself after this many calls (0: never)
    #[serde(default, skip_serializing_if = "is_zero_u")]
    pub max: u32,
    /// ms; stopped, waiting for the review
    #[serde(default, skip_serializing_if = "is_zero_i")]
    pub stopped: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub paused: bool,
    /// More body fields masked in what is kept (secrets, card numbers, CPF and e-mails always are).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mask: Vec<String>,
}
fn every() -> String {
    "every".into()
}
fn is_zero_i(x: &i64) -> bool {
    *x == 0
}
fn is_zero_u(x: &u32) -> bool {
    *x == 0
}

fn v12() -> String {
    "1.2".into()
}
fn s200() -> u16 {
    200
}

fn people() -> String {
    "people".into()
}
fn six() -> u8 {
    6
}
fn zeros() -> String {
    "000000".into()
}
fn three() -> u32 {
    3
}
fn s180() -> u32 {
    180
}
fn authorize_deny() -> Vec<String> {
    vec!["Authorize".into(), "Deny".into()]
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct PersonField {
    /// The data table the people come from.
    #[serde(default = "people")]
    pub from: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct OtpField {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    #[serde(default = "six")]
    pub digits: u8,
    /// This code is refused on purpose.
    #[serde(default = "zeros")]
    pub refuse: String,
    /// Wrong codes before the page locks.
    #[serde(default = "three")]
    pub tries: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct TextField {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct ChoiceField {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub options: Vec<String>,
}

/// One field of a page. Pages are declared, never HTML: Stuntman draws these and nothing else.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Field {
    Person(PersonField),
    Otp(OtpField),
    Text(TextField),
    Choice(ChoiceField),
}

/// A push approval instead of the page: the app gets an answer at once and polls a `step` route
/// while someone approves on "the phone" (a Stuntman page, or approve_push).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Push {
    /// When the request is a push (query.redirect_uri == 'push://').
    pub when: String,
    /// The person's CPF (query.login_hint).
    pub person: String,
    /// The answer sent at once; it reads {{ step.code }}.
    pub answer: String,
    #[serde(default = "s180")]
    pub seconds: u32,
}

/// A page the provider shows in the browser (login, consent, approval), served by Stuntman.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct Page {
    /// "GET /v0/oauth/authorize"
    pub route: String,
    pub title: String,
    /// A template: "{{ query.client_id }} wants to sign".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ask: String,
    #[serde(default, with = "serde_yaml::with::singleton_map_recursive")]
    pub fields: Vec<Field>,
    /// The first one authorizes (redirect with code and state); the second denies (error=access_denied).
    #[serde(default = "authorize_deny")]
    pub buttons: Vec<String>,
    /// pkce_s256 | state | redirect_uri: refused on the page when missing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<Push>,
}

impl Page {
    pub fn method_path(&self) -> (String, String) {
        Route { route: self.route.clone(), ..Default::default() }.method_path()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default)]
pub struct Plugin {
    pub service: Service,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
    #[serde(default)]
    pub answers: Vec<Answer>,
    /// Data tables the answers read: irregular_cpfs, people, selfies…
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, Value>,
    /// Pages Stuntman draws for this provider (authorize, approval).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<Page>,
    /// A SOAP service (A7): envelope, WSDL, types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soap: Option<Soap>,
    /// The SOAP operations, with the same steps as routes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub operations: Vec<Route>,
    /// Made from an OpenAPI spec (A8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spec: Option<Spec>,
    /// Recording the real provider (A9).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<Record>,
    /// The service is an MCP server too (A10b).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<Mcp>,
}

/// Generic answers every service has (A1 "next calls answer", A3 route `fail`).
pub const GENERIC: [(&str, &str); 12] = [("session_expired", "MCP: the session is gone (404)"), ("cut", "The stream stops halfway"), ("slow", "2 s between stream parts"), ("429", "429 Too many requests"), ("reset", "Connection reset"), ("empty", "Empty reply"), ("timeout", "Timeout (no answer)"), ("401", "401 Unauthorized, empty body"), ("403", "403 Forbidden, empty body"), ("500", "500 Internal error"), ("503", "503 Unavailable"), ("html", "HTML error page with 200")];

pub fn dir(ctx: &Ctx) -> std::path::PathBuf {
    ctx.path("apis")
}

pub fn file(ctx: &Ctx, name: &str) -> std::path::PathBuf {
    dir(ctx).join(format!("{name}.yaml"))
}

pub fn rel(name: &str) -> String {
    format!("apis/{name}.yaml")
}

pub fn valid_name(n: &str) -> Result<(), ApiError> {
    if n.is_empty() || n.len() > 48 || !n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        return Err(ApiError::bad("Service name: lowercase letters, numbers and dashes, like serpro-datavalid"));
    }
    Ok(())
}

pub fn text(ctx: &Ctx, name: &str) -> String {
    std::fs::read_to_string(file(ctx, name)).unwrap_or_default()
}

pub fn version(t: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    t.hash(&mut h);
    format!("{:016x}", h.finish())
}

pub fn parse(t: &str) -> Result<Plugin, String> {
    serde_yaml::from_str(t).map_err(|e| match e.location() {
        Some(l) => format!("line {}: {}", l.line(), e.to_string().split(" at line").next().unwrap_or("")),
        None => e.to_string(),
    })
}

pub fn get(ctx: &Ctx, name: &str) -> Result<Plugin, ApiError> {
    valid_name(name)?;
    let t = std::fs::read_to_string(file(ctx, name)).map_err(|_| ApiError::not_found(format!("No API service “{name}” in {}", ctx.env)))?;
    parse(&t).map_err(|e| ApiError::bad(format!("{}: {e}", rel(name))))
}

/// Every plugin that loads; broken files are reported by list_api_services.
pub fn all(ctx: &Ctx) -> Vec<Plugin> {
    let mut v: Vec<Plugin> = std::fs::read_dir(dir(ctx)).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok().and_then(|t| parse(&t).ok()))
        .collect();
    v.sort_by(|a, b| a.service.name.cmp(&b.service.name));
    v
}

pub fn broken(ctx: &Ctx) -> Vec<(String, String)> {
    std::fs::read_dir(dir(ctx)).into_iter().flatten().flatten()
        .filter(|e| e.path().extension().map(|x| x == "yaml").unwrap_or(false))
        .filter_map(|e| {
            let t = std::fs::read_to_string(e.path()).ok()?;
            parse(&t).err().map(|err| (e.file_name().to_string_lossy().trim_end_matches(".yaml").to_string(), err))
        })
        .collect()
}

/// Line of the first occurrence of `needle` in the file (for errors "at line N").
fn line_of(t: &str, needle: &str) -> Option<usize> {
    t.lines().position(|l| l.contains(needle)).map(|i| i + 1)
}

/// Schema and expression errors with line numbers.
pub fn validate(p: &Plugin, t: &str) -> Vec<Value> {
    let mut out = vec![];
    let mut err = |msg: String, needle: &str| out.push(json!({"error": msg, "line": line_of(t, needle)}));
    if p.service.name.is_empty() {
        err("service.name is required".into(), "service:");
    }
    let names: Vec<&str> = p.answers.iter().map(|a| a.name.as_str()).collect();
    let mut seen = std::collections::HashSet::new();
    if !p.operations.is_empty() && p.soap.is_none() {
        err("operations need a soap: block (version, endpoint)".into(), "operations:");
    }
    if let Some(sp) = &p.soap {
        if !["1.1", "1.2", "both"].contains(&sp.version.as_str()) {
            err("soap.version: \"1.1\" | \"1.2\" | both".into(), "version:");
        }
        if !sp.endpoint.starts_with('/') {
            err("soap.endpoint: /path".into(), "endpoint:");
        }
        if !["", "none", "any", "username_token"].contains(&sp.security.as_str()) {
            err("soap.security: none | any | username_token".into(), "security:");
        }
    }
    for r in p.routes.iter().chain(p.operations.iter()) {
        let (m, path) = r.method_path();
        if let Some(re) = path.strip_prefix('~') {
            if let Err(e) = fancy_regex::Regex::new(re) {
                err(format!("route “{}”: the path regex: {e}", r.route), &r.route);
            }
        }
        if !(r.route.is_empty() && (!r.operation.is_empty() || !r.tool.is_empty())) && (!["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"].contains(&m.as_str()) || !(path.starts_with('/') || path.starts_with('~'))) {
            err(format!("route “{}”: METHOD /path", r.route), &r.route);
        }
        if !seen.insert(format!("{m} {path} {}", r.graphql)) {
            err(format!("“{}” is there twice", if !r.route.is_empty() { &r.route } else { &r.operation }), if !r.route.is_empty() { &r.route } else { &r.operation });
        }
        for (k, e) in &r.lets {
            if let Err(x) = expr::parse(e) {
                err(format!("let {k}: {x}"), e);
            }
        }
        if let Some(k) = &r.keep {
            if let Err(x) = expr::parse(&k.key) {
                err(format!("keep.key: {x}"), &k.key);
            }
        }
        let lets: Vec<&str> = r.lets.keys().map(String::as_str).collect();
        for d in &r.decide {
            if let Some(w) = &d.when {
                if let Err(x) = expr::parse(w) {
                    err(format!("{w}: {x}"), w);
                } else {
                    // A field read on a let that the data tables don't have: "No field tg on person".
                    for p2 in expr::paths(w) {
                        let mut parts = p2.split('.');
                        let root = parts.next().unwrap_or("");
                        if let (true, Some(field)) = (lets.contains(&root), parts.next()) {
                            if let Some(table) = r.lets.get(root).and_then(|e| e.split('\'').nth(1).or(e.split('"').nth(1))) {
                                let cols: Vec<String> = p.data.get(table).and_then(|t| t.as_array()).and_then(|a| a.first()).and_then(|row| row.as_object()).map(|o| o.keys().cloned().collect()).unwrap_or_default();
                                if !cols.is_empty() && !cols.iter().any(|c| c == field) {
                                    let close = cols.iter().min_by_key(|c| strsim(c, field)).cloned().unwrap_or_default();
                                    err(format!("No field “{field}” on {root}. Did you mean “{close}”?"), w);
                                }
                            }
                        }
                    }
                }
            }
            if !names.contains(&d.answer.as_str()) && !GENERIC.iter().any(|g| g.0 == d.answer) {
                err(format!("decide → “{}”: no such answer", d.answer), &format!("answer: {}", d.answer));
            }
        }
        if let Some(w) = &r.wait {
            for a in [&w.answer, &w.then] {
                if !names.contains(&a.as_str()) {
                    err(format!("wait → “{a}”: no such answer"), &format!("answer: {a}"));
                }
            }
        }
        if !r.fail.is_empty() && !GENERIC.iter().any(|g| g.0 == r.fail) {
            err(format!("fail: {}", GENERIC.iter().map(|g| g.0).collect::<Vec<_>>().join(" | ")), "fail:");
        }
        if !["", "authorization_code", "password"].contains(&r.grant.as_str()) {
            err("grant: authorization_code | password".into(), "grant:");
        }
        if !r.step.is_empty() {
            if let Err(x) = expr::parse(&r.step) {
                err(format!("step: {x}"), &r.step);
            }
        }
        if !r.native.is_empty() {
            match crate::restpki::NATIVE.iter().find(|n| n.0 == r.native) {
                None => err(format!("native: {}", crate::restpki::NATIVE.iter().map(|n| n.0).collect::<Vec<_>>().join(" | ")), "native:"),
                Some((_, answers)) => {
                    for a in answers.iter().filter(|a| !names.contains(a)) {
                        err(format!("native {} answers “{a}”: no such answer", r.native), &r.native);
                    }
                }
            }
        }
        if !r.receive.invalid.is_empty() && !names.contains(&r.receive.invalid.as_str()) {
            err(format!("receive.invalid → “{}”: no such answer", r.receive.invalid), "invalid:");
        }
        if !r.receive.schema.is_empty() && !p.spec.as_ref().map(|s| s.schemas.contains_key(&r.receive.schema)).unwrap_or(false) {
            err(format!("receive.schema “{}”: no such schema in spec.schemas", r.receive.schema), "schema:");
        }
        if !r.receive.auth.is_empty() && !["none", "bearer", "basic", "session"].contains(&r.receive.auth.as_str()) {
            err("receive.auth: none | bearer | basic | session".into(), "auth:");
        }
        for s in &r.send {
            if let Err(x) = expr::check_template(&s.url).and_then(|_| expr::check_template(&s.body)).and_then(|_| if s.when.is_empty() { Ok(()) } else { expr::parse(&s.when) }) {
                err(format!("send: {x}"), &s.url);
            }
            if !s.sign.is_empty() && !SIGNS.contains(&s.sign.as_str()) {
                err(format!("send.sign: {}", SIGNS.join(" | ")), "sign:");
            }
        }
        if !r.tool.is_empty() && p.mcp.is_none() {
            err(format!("tool “{}” needs an mcp: block", r.tool), &r.tool);
        }
    }
    if let Some(w) = &p.service.webhooks {
        if !SIGNS.contains(&w.sign.as_str()) {
            err(format!("webhooks.sign: {}", SIGNS.join(" | ")), "sign:");
        }
        for x in w.provoke.iter().filter(|x| !SIGN_PROVOKE.contains(&x.as_str())) {
            err(format!("webhooks.provoke “{x}”: {}", SIGN_PROVOKE.join(" | ")), x);
        }
    }
    for a in &p.answers {
        if !["", "sse", "ndjson", "chunks"].contains(&a.stream_format.as_str()) {
            err("stream_format: sse | ndjson | chunks".into(), "stream_format:");
        }
        for pt in &a.stream {
            if let Err(x) = expr::check_template(&pt.data) {
                err(format!("answer {} stream: {x}", a.name), &pt.data);
            }
            if !pt.after.is_empty() && stuntman_core::util::parse_ms(&pt.after).is_none() {
                err(format!("answer {} stream after: “{}” (400ms, 1.2s)", a.name, pt.after), &pt.after);
            }
        }
    }
    for pg in &p.pages {
        let (m, path) = pg.method_path();
        if m != "GET" || !path.starts_with('/') {
            err(format!("page “{}”: GET /path", pg.route), &pg.route);
        }
        if let Err(x) = expr::check_template(&pg.ask) {
            err(format!("page ask: {x}"), &pg.ask);
        }
        for c in &pg.checks {
            if !["pkce_s256", "state", "redirect_uri"].contains(&c.as_str()) {
                err(format!("checks: “{c}”; known: pkce_s256, state, redirect_uri"), c);
            }
        }
        if pg.buttons.is_empty() || pg.buttons.len() > 2 {
            err("buttons: one or two (authorize, then deny)".into(), "buttons:");
        }
        for f in &pg.fields {
            if let Field::Person(pf) = f {
                if !p.data.contains_key(&pf.from) {
                    err(format!("person.from: no data table “{}”", pf.from), &pf.from);
                }
            }
        }
        if let Some(pu) = &pg.push {
            for e in [&pu.when, &pu.person] {
                if let Err(x) = expr::parse(e) {
                    err(format!("push: {x}"), e);
                }
            }
            if !names.contains(&pu.answer.as_str()) {
                err(format!("push.answer “{}”: no such answer", pu.answer), &pu.answer);
            }
        }
    }
    for a in &p.answers {
        let body_ok = if a.raw { Ok(()) } else if a.template == "handlebars" { crate::hbs::check(&a.body) } else { expr::check_template(&a.body) };
        if !["", "handlebars"].contains(&a.template.as_str()) {
            err(format!("answer {}: template: handlebars (or nothing)", a.name), &format!("name: {}", a.name));
        }
        if let Err(x) = body_ok.and_then(|_| expr::check_template(&a.sentence)).and_then(|_| expr::check_template(&a.fault.as_ref().map(|f| f.reason.clone()).unwrap_or_default())) {
            err(format!("answer {}: {x}", a.name), &format!("name: {}", a.name));
        }
    }
    if let Some(rc) = &p.record {
        if !rc.upstream.starts_with("http://") && !rc.upstream.starts_with("https://") {
            err("record.upstream: the provider's URL, https://…".into(), "upstream:");
        }
        if !["every", "unknown"].contains(&rc.mode.as_str()) {
            err("record.mode: every | unknown".into(), "mode:");
        }
    }
    if p.service.limits.fail_share < 0.0 || p.service.limits.fail_share > 1.0 {
        err("limits.fail_share: 0 to 1".into(), "fail_share");
    }
    out
}

fn strsim(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push((prev[j] + (ca != *cb) as usize).min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Saves after validating; refuses a file with errors.
pub fn save(ctx: &Ctx, p: &Plugin) -> Result<String, ApiError> {
    valid_name(&p.service.name)?;
    let t = serde_yaml::to_string(p).map_err(|e| ApiError::internal(e.to_string()))?;
    save_text(ctx, &p.service.name, &t)
}

pub fn save_text(ctx: &Ctx, name: &str, t: &str) -> Result<String, ApiError> {
    let p = parse(t).map_err(|e| ApiError::bad(format!("{}: {e}", rel(name))))?;
    let errs = validate(&p, t);
    if !errs.is_empty() {
        return Err(ApiError::bad(format!("{}: {}", rel(name), errs[0]["error"].as_str().unwrap_or(""))).with(json!({"errors": errs})));
    }
    if p.service.name != name {
        return Err(ApiError::bad(format!("service.name is “{}” but the file is {}", p.service.name, rel(name))));
    }
    std::fs::create_dir_all(dir(ctx)).map_err(|e| ApiError::internal(e.to_string()))?;
    std::fs::write(file(ctx, name), t).map_err(|e| ApiError::internal(e.to_string()))?;
    ctx.live.send("apis.plugin", name, "updated");
    Ok(version(t))
}

/// "300ms..600ms" → (300, 600)
pub fn latency(s: &str) -> (u64, u64) {
    let s = s.replace(' ', "");
    match s.split_once("..") {
        Some((a, b)) => (stuntman_core::util::parse_ms(a).unwrap_or(0), stuntman_core::util::parse_ms(b).unwrap_or(0)),
        None => {
            let x = stuntman_core::util::parse_ms(&s).unwrap_or(0);
            (x, x)
        }
    }
}

/// "10/s" → 10 per 1000 ms; "100/day" → 100 per day.
pub fn per(s: &str) -> Option<(u64, i64)> {
    let (n, u) = s.split_once('/')?;
    let w = match u.trim() { "s" | "sec" => 1000, "m" | "min" => 60_000, "h" | "hour" => 3_600_000, "d" | "day" => 86_400_000, _ => return None };
    Some((n.trim().parse().ok()?, w))
}

/// The catalog: plugin files shipped with Stuntman (engine/plugins/*.yaml).
pub fn catalog_dir() -> std::path::PathBuf {
    std::env::var("STUNTMAN_PLUGINS").map(std::path::PathBuf::from).unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins"))
}

#[cfg(test)]
mod t {
    use super::*;

    #[test]
    fn validation_names_the_line() {
        let t = "service:\n  name: s\nroutes:\n  - route: POST /v\n    let:\n      person: lookup('people', 'cpf', body.cpf)\n    decide:\n      - when: person.tg == 'x'\n        answer: ok\nanswers:\n  - name: ok\ndata:\n  people:\n    - {cpf: '1', tag: a}\n";
        let p = parse(t).unwrap();
        let e = validate(&p, t);
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0]["error"].as_str().unwrap().contains("Did you mean “tag”"));
        assert_eq!(e[0]["line"], 8);
        assert_eq!(latency("300ms..600ms"), (300, 600));
        assert_eq!(per("10/s"), Some((10, 1000)));
    }
}
