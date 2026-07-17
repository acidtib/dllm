import { useQuery } from "@tanstack/react-query";
import { fetchStatus, fetchInferencePolicy } from "../lib/client";
import { fmtPubkey } from "../lib/utils";
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
  const ownerKey = fmtPubkey(network.owner_pubkey);

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Overview</h2>

      <p className="text-sm">
        Health:{" "}
        <span
          className={
            data.health === "ready"
              ? "text-ready font-medium"
              : data.health === "degraded" || data.health === "unavailable"
                ? "text-unavailable font-medium"
                : "text-degraded font-medium"
          }
        >
          {data.health}
        </span>
      </p>

      <div className="grid grid-cols-3 gap-4">
        <StatCard label="Network" value={network.name} />
        <StatCard label="Generation" value={String(network.generation)} />
        <StatCard label="Owner" value={ownerKey} mono />
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
        <AssignModelSection ownerPubkey={network.owner_pubkey} />
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
