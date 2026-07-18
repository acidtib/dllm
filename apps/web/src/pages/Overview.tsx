import { useQuery } from "@tanstack/react-query";
import { fetchStatus, fetchInferencePolicy } from "../lib/client";
import { cn, healthClass } from "../lib/utils";
import { PubkeyBadge } from "../components/ui/pubkey-badge";
import { InvitationSection, AssignModelSection, RecoveryNote } from "./AdminActions";

export function Overview() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["status"],
    queryFn: fetchStatus,
    refetchInterval: 10_000,
  });

  const { data: policies } = useQuery({
    queryKey: ["inference-policy"],
    queryFn: fetchInferencePolicy,
  });

  if (isLoading) {
    return <p className="text-gray-400">Loading status...</p>;
  }

  if (error || !data) {
    return (
      <p className="text-unavailable">
        Status unavailable: {error?.message || "Unknown error"}
      </p>
    );
  }

  const network = data.network.state;

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Overview</h2>

      <p className="text-sm">
        Health:{" "}
        <span className={cn(healthClass(data.health), "font-medium")}>
          {data.health}
        </span>
      </p>

      {network.members.length <= 1 && (
        <div className="rounded-lg border border-dashed border-accent/50 bg-accent/10 p-4 text-sm">
          <p className="font-medium">Getting started</p>
          <p className="mt-1 text-gray-300">
            This network only has its owner node so far. Generate an invitation
            below and run{" "}
            <code className="rounded bg-gray-950 px-1 py-0.5 font-mono text-xs">
              dllm onboard &lt;url&gt;
            </code>{" "}
            on another machine to add a peer.
          </p>
        </div>
      )}

      <div className="grid grid-cols-3 gap-4">
        <StatCard label="Network" value={network.name} />
        <StatCard label="Generation" value={String(network.generation)} />
        <div className="rounded-lg border border-border bg-surface p-4">
          <p className="mb-1 text-xs text-gray-400">Authority</p>
          <PubkeyBadge bytes={network.authority_pubkey} className="text-sm" />
        </div>
      </div>

      <div className="grid grid-cols-4 gap-4">
        <StatCard label="Members" value={String(network.members.length)} />
        <StatCard label="Nodes" value={String(data.nodes.length)} />
        <StatCard label="Workers" value={String(data.workers.length)} />
        <StatCard label="Placements" value={String(data.placements.length)} />
      </div>

      <div className="grid grid-cols-2 gap-4">
        <StatCard
          label="Hardware Profiles"
          value={String(network.hardware_profiles?.length ?? 0)}
        />
        <StatCard
          label="Network ID"
          value={network.network_id}
          mono
        />
      </div>

      {policies && policies.length > 0 && (
        <div className="rounded-lg border border-border bg-surface p-4">
          <h3 className="mb-2 text-sm font-semibold">Inference Policy</h3>
          {policies.map((p, i) => (
            <p key={i} className="text-xs text-gray-400">
              {p.label}: {p.max_in_flight} concurrent
            </p>
          ))}
        </div>
      )}

      <div className="grid grid-cols-2 gap-4">
        <InvitationSection />
        <AssignModelSection authorityPubkey={network.authority_pubkey} />
      </div>
      <RecoveryNote />
    </div>
  );
}

function StatCard({
  label,
  value,
  mono,
}: {
  label: string;
  value: string;
  mono?: boolean;
}) {
  return (
    <div className="rounded-lg border border-border bg-surface p-4">
      <p className="mb-1 text-xs text-gray-400">{label}</p>
      <p className={mono ? "font-mono text-sm" : "text-sm font-medium"}>
        {value}
      </p>
    </div>
  );
}
