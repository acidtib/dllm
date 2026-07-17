// ---- enums ----

export type HealthState = "ready" | "unknown" | "degraded" | "unavailable";

export type TransportKind = "local" | "direct" | "relay";

export type PlacementLifecycle = "ready" | "draining";

export type ManagementRole = "viewer" | "operator" | "admin";

// ---- network state ----

export interface NetworkState {
  schema_version: number;
  network_id: string;
  name: string;
  owner_pubkey: number[];
  generation: number;
  members: Member[];
  model_assignments: ModelAssignment[];
  placements: Placement[];
  hardware_profiles?: HardwareProfile[];
  transport_bindings?: TransportEndpointBinding[];
  transport_revocations?: TransportEndpointRevocation[];
  forwarding_policy?: ForwardingPolicy[];
  resource_budgets?: ResourceBudget[];
  banned?: MembershipBan[];
}

export interface SignedState {
  state: NetworkState;
  signature: number[];
}

export interface Member {
  node_pubkey: number[];
  endpoint: string;
  relay_endpoint?: string | null;
  joined_generation: number;
}

export interface ModelAssignment {
  model: string;
  node_pubkey: number[];
}

export interface Placement {
  placement_id: string;
  model: string;
  node_pubkey: number[];
  created_generation: number;
  lifecycle: PlacementLifecycle;
}

export interface ForwardingPolicy {
  node_pubkey: number[];
  max_reservations: number;
}

export interface ResourceBudget {
  node_pubkey: number[];
  max_in_flight: number;
  max_requests_per_window: number;
  window_seconds: number;
  granted_generation: number;
}

export interface MembershipBan {
  node_pubkey: number[];
  banned_at_unix: number;
  reason: string;
}

export interface AccessRequest {
  node_pubkey: number[];
  requested_endpoint: string;
  note: string;
  requested_at_unix: number;
}

export interface SignedAccessRequest {
  request: AccessRequest;
  signature: number[];
}

export interface AbuseReport {
  reporter_pubkey: number[];
  subject_pubkey: number[];
  category: string;
  note: string;
  reported_at_unix: number;
}

export interface TransportEndpointBinding {
  node_pubkey: number[];
  transport_peer_id: string;
  binding_generation: number;
  issued_at_unix: number;
  expires_at_unix: number;
}

export interface TransportEndpointRevocation {
  node_pubkey: number[];
  transport_peer_id: string;
  binding_generation: number;
  revoked_at_unix: number;
}

// ---- hardware ----

export interface HardwareProfile {
  node_pubkey: number[];
  observed_at_unix: number;
  cpu: CpuCapability;
  system_memory_bytes: number;
  available_memory_bytes: number;
  accelerators: AcceleratorCapability[];
  runtimes: RuntimeCapability[];
  benchmarks: HardwareBenchmark[];
}

export interface CpuCapability {
  model: string;
  physical_cores: number;
  logical_cores: number;
  features: string[];
}

export interface AcceleratorCapability {
  backend: string;
  device_name: string;
  device_id: string;
  driver: string;
  memory_bytes?: number | null;
}

export interface RuntimeCapability {
  runtime: string;
  revision: string;
  backend: string;
  architectures: string[];
}

export interface HardwareBenchmark {
  model: string;
  backend: string;
  context_size: number;
  concurrency: number;
  prompt_tokens_per_second_milli: number;
  decode_tokens_per_second_milli: number;
  peak_memory_bytes: number;
}

// ---- status types ----

export interface NodeStatus {
  node_pubkey: number[];
  endpoint: string;
  owner: boolean;
  health: HealthState;
  transport: TransportKind | null;
}

export interface WorkerStatus {
  worker_id: string;
  node_pubkey: number[];
  model: string;
  health: HealthState;
}

export interface PlacementStatus {
  placement_id: string;
  model: string;
  generation: number;
  worker_ids: string[];
  health: HealthState;
  lifecycle: PlacementLifecycle;
}

export interface ManagementStatus {
  network: SignedState;
  nodes: NodeStatus[];
  workers: WorkerStatus[];
  placements: PlacementStatus[];
  health: HealthState;
}

// ---- peer network ----

export interface PeerNetworkStatus {
  enabled: boolean;
  peer_id: string | null;
  discovery_mode: string;
  published_discovery: boolean;
  dht_hosting: boolean;
  forwarding_enabled: boolean;
  path: string | null;
  selected_forwarder: string | null;
  reservation_active: boolean;
  bootstrap_peers: string[];
  discovered_providers: string[];
  active_inbound_streams: number;
  active_outbound_streams: number;
  rejected_streams: number;
  cancelled_streams: number;
  deadline_expirations: number;
  protocol_failures: number;
  auth_failures: number;
  failed_connections: number;
  reselections: number;
  last_error: string | null;
}

// ---- credentials ----

export interface CredentialSummary {
  id: string;
  label: string;
  role: ManagementRole;
  revocable: boolean;
}

export interface CreatedCredential {
  id: string;
  token: string;
}

// ---- inference ----

export interface InferencePolicy {
  label: string;
  max_in_flight: number;
}

// ---- placement preview ----

export interface PlacementPreviewRequest {
  model: string;
  architecture: string;
  required_memory_bytes: number;
  compatible_backends: string[];
}

export interface PlacementPreviewCandidate {
  compatible: boolean;
  backend: string;
  memory_headroom_bytes: number;
  decode_tokens_per_second_milli: number | null;
  explanations: string[];
}

export interface PlacementPreviewResponse {
  candidates: PlacementPreviewCandidate[];
}

// ---- join token ----

export interface JoinToken {
  token: string;
  network_id: string;
  expires_at_unix: number | null;
}

export interface SignedJoinToken {
  token: JoinToken;
  signature: number[];
}
