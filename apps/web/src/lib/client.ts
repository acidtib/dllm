import { get, post, del, getToken, apiPath } from "./api";
import type {
  ManagementStatus,
  PeerNetworkStatus,
  InferencePolicy,
  PlacementPreviewRequest,
  PlacementPreviewResponse,
  CredentialSummary,
  CreatedCredential,
  ManagementRole,
  AccessRequest,
  AbuseReport,
  JoinToken,
  SignedJoinToken,
  HardwareBenchmark,
} from "./types";

// ---- viewer endpoints ----

export function fetchStatus(): Promise<ManagementStatus> {
  return get<ManagementStatus>("/v1/status");
}

export function fetchPeerNetworkStatus(): Promise<PeerNetworkStatus> {
  return get<PeerNetworkStatus>("/v1/peer-network/status");
}

export function fetchInferencePolicy(): Promise<InferencePolicy[]> {
  return get<InferencePolicy[]>("/v1/inference-policy");
}

export function previewPlacement(
  req: PlacementPreviewRequest,
): Promise<PlacementPreviewResponse> {
  return post<PlacementPreviewResponse>("/v1/placements/preview", req);
}

export function fetchAccessRequests(): Promise<AccessRequest[]> {
  return get<AccessRequest[]>("/v1/access-requests");
}

export function fetchAuditLog(limit = 20): Promise<
  {
    timestamp_unix: number;
    actor: string;
    action: string;
    target: string;
    outcome: string;
  }[]
> {
  return get(`/v1/audit-log?limit=${limit}`);
}

// ---- operator endpoints ----

export function assignModel(model: string, nodePubkey: number[]): Promise<void> {
  return post<void>("/v1/assignments", { model, node_pubkey: nodePubkey });
}

export function unassignModel(model: string, nodePubkey: number[]): Promise<void> {
  return del<void>("/v1/assignments", { model, node_pubkey: nodePubkey });
}

export function publishHardwareProfile(profile: {
  cpu: { model: string; physical_cores: number; logical_cores: number; features: string[] };
  system_memory_bytes: number;
  available_memory_bytes: number;
  accelerators: { backend: string; device_name: string; device_id: string; driver: string; memory_bytes?: number | null }[];
  runtimes: { runtime: string; revision: string; backend: string; architectures: string[] }[];
  benchmarks: HardwareBenchmark[];
}): Promise<void> {
  return post<void>("/v1/hardware-profiles", profile);
}

export function drainPlacement(placementId: string): Promise<void> {
  return post<void>(`/v1/placements/${placementId}/drain`);
}

export function resumePlacement(placementId: string): Promise<void> {
  return del<void>(`/v1/placements/${placementId}/drain`);
}

// ---- admin: invitations ----

export function createInvitation(expiresAtUnix?: number | null): Promise<JoinToken> {
  return post<JoinToken>("/v1/invitations", {
    expires_at_unix: expiresAtUnix ?? null,
  });
}

// ---- admin: members ----

export function revokeMember(nodePubkey: number[]): Promise<void> {
  return post<void>("/v1/members/revoke", { node_pubkey: nodePubkey });
}

// ---- admin: transport bindings ----

export function bindTransport(
  nodePubkey: number[],
  transportPeerId: string,
  bindingGeneration: number,
  expiresAtUnix: number,
): Promise<void> {
  return post<void>("/v1/transport-bindings", {
    node_pubkey: nodePubkey,
    transport_peer_id: transportPeerId,
    binding_generation: bindingGeneration,
    expires_at_unix: expiresAtUnix,
  });
}

export function revokeTransport(
  nodePubkey: number[],
  transportPeerId: string,
): Promise<void> {
  return post<void>("/v1/transport-bindings/revoke", {
    node_pubkey: nodePubkey,
    transport_peer_id: transportPeerId,
  });
}

// ---- admin: forwarding policy ----

export function setForwardingPolicy(
  nodePubkey: number[],
  maxReservations: number | null,
): Promise<void> {
  return post<void>("/v1/forwarding-policy", {
    node_pubkey: nodePubkey,
    max_reservations: maxReservations,
  });
}

// ---- admin: access requests ----

export function approveAccessRequest(
  nodePubkey: number[],
  endpoint?: string,
): Promise<void> {
  return post<void>("/v1/access-requests/approve", {
    node_pubkey: nodePubkey,
    endpoint: endpoint ?? null,
  });
}

export function denyAccessRequest(nodePubkey: number[]): Promise<void> {
  return post<void>("/v1/access-requests/deny", {
    node_pubkey: nodePubkey,
  });
}

// ---- admin: resource budgets ----

export function setResourceBudget(
  nodePubkey: number[],
  maxInFlight: number,
  maxRequestsPerWindow: number,
  windowSeconds: number,
): Promise<void> {
  return post<void>("/v1/resource-budgets", {
    node_pubkey: nodePubkey,
    max_in_flight: maxInFlight,
    max_requests_per_window: maxRequestsPerWindow,
    window_seconds: windowSeconds,
  });
}

export function removeResourceBudget(nodePubkey: number[]): Promise<void> {
  return del<void>("/v1/resource-budgets", { node_pubkey: nodePubkey });
}

// ---- admin: credentials ----

export function fetchCredentials(): Promise<CredentialSummary[]> {
  return get<CredentialSummary[]>("/v1/management/credentials");
}

export function createCredential(
  label: string,
  role: ManagementRole,
): Promise<CreatedCredential> {
  return post<CreatedCredential>("/v1/management/credentials", { label, role });
}

export function revokeCredential(credentialId: string): Promise<void> {
  return del<void>(`/v1/management/credentials/${credentialId}`);
}

// ---- admin: moderation ----

export function banNode(nodePubkey: number[], reason: string): Promise<void> {
  return post<void>("/v1/moderation/bans", {
    node_pubkey: nodePubkey,
    reason,
  });
}

export function unbanNode(nodePubkey: number[]): Promise<void> {
  return del<void>("/v1/moderation/bans", { node_pubkey: nodePubkey });
}

// ---- admin: abuse reports ----

export function fetchAbuseReports(): Promise<AbuseReport[]> {
  return get<AbuseReport[]>("/v1/abuse-reports");
}

export function submitAbuseReport(report: {
  subject_pubkey: number[];
  category: string;
  note: string;
}): Promise<void> {
  return post<void>("/v1/abuse-reports", {
    report: {
      ...report,
      reported_at_unix: Math.floor(Date.now() / 1000),
    },
  });
}

// ---- public endpoints ----

export function joinNetwork(token: SignedJoinToken, nodePubkey: number[], nodeEndpoint: string): Promise<void> {
  return post<void>("/v1/members/join", {
    token,
    node_pubkey: nodePubkey,
    node_endpoint: nodeEndpoint,
  });
}

export function submitAccessRequest(req: {
  node_pubkey: number[];
  requested_endpoint: string;
  note: string;
}): Promise<void> {
  return post<void>("/v1/access-requests", {
    request: {
      ...req,
      requested_at_unix: Math.floor(Date.now() / 1000),
    },
  });
}

// ---- inference proxy ----

export function fetchModels(): Promise<{ object: string; data: { id: string; object: string; created: number; owned_by: string }[] }> {
  return get("/v1/models");
}

export async function chatCompletion(body: {
  model: string;
  messages: { role: string; content: string }[];
  stream?: boolean;
  temperature?: number;
  max_tokens?: number;
}): Promise<Response> {
  const token = getToken();
  const headers: Record<string, string> = {
    "content-type": "application/json",
    ...(token ? { Authorization: `Bearer ${token}` } : {}),
  };
  return fetch(apiPath("/v1/chat/completions"), {
    method: "POST",
    headers,
    body: JSON.stringify(body),
  });
}
