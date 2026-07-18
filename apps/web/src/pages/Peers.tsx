import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  fetchStatus,
  fetchPeerNetworkStatus,
  bindTransport,
  revokeTransport,
  setForwardingPolicy,
} from "../lib/client";
import type { NodeStatus } from "../lib/types";
import { bytesToHex, fmtPubkey, fmtUnix, hexToBytes } from "../lib/utils";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { PubkeyBadge } from "../components/ui/pubkey-badge";

export function Peers() {
  const queryClient = useQueryClient();
  const { data, isLoading, error } = useQuery({
    queryKey: ["status"],
    queryFn: fetchStatus,
    refetchInterval: 10_000,
  });

  const { data: peer } = useQuery({
    queryKey: ["peer-network"],
    queryFn: fetchPeerNetworkStatus,
    refetchInterval: 10_000,
  });

  if (isLoading) return <p className="text-gray-400">Loading peers...</p>;
  if (error || !data) {
    return (
      <p className="text-unavailable">
        Peers unavailable: {error?.message || "Unknown error"}
      </p>
    );
  }

  const network = data.network.state;
  const bindings = network.transport_bindings || [];
  const fwds = network.forwarding_policy || [];

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Peer Network</h2>

      {peer ? (
        <div className="grid grid-cols-2 gap-2 rounded-lg border border-border bg-surface p-4 text-sm">
          <KV label="Enabled" value={peer.enabled ? "yes" : "no"} />
          <KV label="Peer ID" value={peer.peer_id || "none"} mono />
          <KV label="Discovery" value={`${peer.discovery_mode} (published: ${peer.published_discovery ? "yes" : "no"})`} />
          <KV label="DHT" value={peer.dht_hosting ? "server" : "client"} />
          <KV label="Forwarding" value={peer.forwarding_enabled ? "eligible" : "ineligible"} />
          <KV label="Path" value={peer.path || "unknown"} />
          <KV label="Forwarder" value={peer.selected_forwarder || "none, direct"} />
          <KV label="Reservation" value={peer.reservation_active ? "active" : "none"} />
          <KV label="Bootstrap" value={String(peer.bootstrap_peers.length)} />
          <KV label="Discovered" value={String(peer.discovered_providers.length)} />
          <KV label="Streams" value={`${peer.active_inbound_streams} in / ${peer.active_outbound_streams} out`} />
          <KV label="Rejected" value={String(peer.rejected_streams)} />
          <KV label="Cancelled" value={String(peer.cancelled_streams)} />
          <KV label="Deadlines" value={String(peer.deadline_expirations)} />
          <KV label="Protocol fail" value={String(peer.protocol_failures)} />
          <KV label="Auth fail" value={String(peer.auth_failures)} />
          <KV label="Failed conn" value={String(peer.failed_connections)} />
          <KV label="Reselections" value={String(peer.reselections)} />
          <KV label="Last error" value={peer.last_error || "none"} />
        </div>
      ) : (
        <p className="text-gray-400">Peer transport disabled</p>
      )}

      <h3 className="text-lg font-semibold">Transport Bindings</h3>
      {bindings.length === 0 ? (
        <p className="text-gray-400">No transport bindings</p>
      ) : (
        <div className="space-y-2">
          {bindings.map((b) => (
            <div
              key={`${fmtPubkey(b.node_pubkey)}-${b.transport_peer_id}`}
              className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
            >
              <span>
                <PubkeyBadge bytes={b.node_pubkey} /> &rarr;{" "}
                {b.transport_peer_id} (gen {b.binding_generation}, expires{" "}
                {fmtUnix(b.expires_at_unix)})
              </span>
              <BindRevoke
                pubkey={b.node_pubkey}
                peerId={b.transport_peer_id}
                onDone={() => queryClient.invalidateQueries({ queryKey: ["status"] })}
              />
            </div>
          ))}
        </div>
      )}

      <BindTransportForm
        onDone={() => queryClient.invalidateQueries({ queryKey: ["status"] })}
      />

      <h3 className="text-lg font-semibold">Forwarding Policy</h3>
      {fwds.length === 0 ? (
        <p className="text-gray-400">No forwarding policy entries</p>
      ) : (
        <div className="space-y-2">
          {fwds.map((f) => (
            <div
              key={fmtPubkey(f.node_pubkey)}
              className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
            >
              <span>
                <PubkeyBadge bytes={f.node_pubkey} /> max reservations:{" "}
                {f.max_reservations}
              </span>
              <RemoveForwarder
                pubkey={f.node_pubkey}
                onDone={() =>
                  queryClient.invalidateQueries({ queryKey: ["status"] })
                }
              />
            </div>
          ))}
        </div>
      )}

      <SetForwarderForm
        nodes={data.nodes}
        onDone={() => queryClient.invalidateQueries({ queryKey: ["status"] })}
      />
    </div>
  );
}

function KV({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <p>
      <span className="text-gray-400">{label}: </span>
      <span className={mono ? "font-mono" : ""}>{value}</span>
    </p>
  );
}

function BindRevoke({
  pubkey,
  peerId,
  onDone,
}: {
  pubkey: number[];
  peerId: string;
  onDone: () => void;
}) {
  const mut = useMutation({
    mutationFn: () => revokeTransport(pubkey, peerId),
    onSuccess: () => {
      onDone();
      toast.success("Transport binding revoked");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Revoke failed"),
  });
  return (
    <Button
      variant="destructive"
      size="sm"
      onClick={() => {
        if (!window.confirm(`Revoke transport binding to ${peerId}?`)) return;
        mut.mutate();
      }}
      disabled={mut.isPending}
    >
      Revoke
    </Button>
  );
}

function BindTransportForm({ onDone }: { onDone: () => void }) {
  const [peerId, setPeerId] = useState("");
  const [gen, setGen] = useState("");
  const [expiry, setExpiry] = useState("");
  const mut = useMutation({
    mutationFn: () =>
      bindTransport(
        [], // node_pubkey inferred server-side from auth
        peerId,
        Number(gen),
        Number(expiry),
      ),
    onSuccess: () => {
      onDone();
      toast.success("Transport bound");
      setPeerId("");
      setGen("");
      setExpiry("");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Bind failed"),
  });

  return (
    <div className="flex gap-2">
      <Input placeholder="libp2p peer ID" value={peerId} onChange={(e) => setPeerId(e.target.value)} />
      <Input placeholder="generation" type="number" value={gen} onChange={(e) => setGen(e.target.value)} />
      <Input placeholder="expires at unix" type="number" value={expiry} onChange={(e) => setExpiry(e.target.value)} />
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Bind
      </Button>
    </div>
  );
}

function SetForwarderForm({
  nodes,
  onDone,
}: {
  nodes: NodeStatus[];
  onDone: () => void;
}) {
  const [hex, setHex] = useState("");
  const [maxRes, setMaxRes] = useState("");
  const mut = useMutation({
    mutationFn: () => {
      const bytes = hexToBytes(hex);
      return setForwardingPolicy(bytes, Number(maxRes));
    },
    onSuccess: () => {
      onDone();
      toast.success("Forwarding policy set");
      setHex("");
      setMaxRes("");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Set failed"),
  });

  return (
    <div className="flex flex-wrap gap-2">
      <select
        value=""
        onChange={(e) => e.target.value && setHex(e.target.value)}
        className="rounded-md border border-border bg-gray-950 px-3 py-1.5 text-sm"
      >
        <option value="">Pick a known node&hellip;</option>
        {nodes.map((n) => (
          <option key={bytesToHex(n.node_pubkey)} value={bytesToHex(n.node_pubkey)}>
            {fmtPubkey(n.node_pubkey)}... {n.endpoint}
          </option>
        ))}
      </select>
      <Input placeholder="node key (hex)" value={hex} onChange={(e) => setHex(e.target.value)} className="min-w-[220px] flex-1" />
      <Input placeholder="max reservations" type="number" value={maxRes} onChange={(e) => setMaxRes(e.target.value)} />
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Set
      </Button>
    </div>
  );
}

function RemoveForwarder({
  pubkey,
  onDone,
}: {
  pubkey: number[];
  onDone: () => void;
}) {
  const mut = useMutation({
    mutationFn: () => setForwardingPolicy(pubkey, null),
    onSuccess: () => {
      onDone();
      toast.success("Forwarder removed");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Remove failed"),
  });
  return (
    <Button
      variant="destructive"
      size="sm"
      onClick={() => {
        if (!window.confirm(`Remove forwarding policy for ${fmtPubkey(pubkey)}...?`)) return;
        mut.mutate();
      }}
      disabled={mut.isPending}
    >
      Remove
    </Button>
  );
}
