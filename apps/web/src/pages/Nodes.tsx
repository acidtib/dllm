import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { fetchStatus, revokeMember } from "../lib/client";
import { fmtPubkey } from "../lib/utils";
import { Button } from "../components/ui/button";

export function Nodes() {
  const queryClient = useQueryClient();
  const { data, isLoading, error } = useQuery({
    queryKey: ["status"],
    queryFn: fetchStatus,
    refetchInterval: 10_000,
  });

  const revokeMut = useMutation({
    mutationFn: (pubkey: number[]) => revokeMember(pubkey),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });

  if (isLoading) return <p className="text-gray-400">Loading nodes...</p>;
  if (error || !data) {
    return (
      <p className="text-unavailable">
        Nodes unavailable: {error?.message || "Unknown error"}
      </p>
    );
  }

  const profiles = data.network.state.hardware_profiles || [];

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Nodes</h2>

      <div className="space-y-3">
        {data.nodes.map((node) => (
          <div
            key={fmtPubkey(node.node_pubkey)}
            className="flex items-center justify-between rounded-lg border border-border bg-surface p-4"
          >
            <div>
              <p className="font-mono text-sm">
                {node.authority ? "[authority] " : ""}
                {fmtPubkey(node.node_pubkey)}...
              </p>
              <p className="text-xs text-gray-400">
                {node.health} &middot; {node.transport || "unknown"} &middot;{" "}
                {node.endpoint}
              </p>
            </div>
            {!node.authority && (
              <Button
                variant="destructive"
                size="sm"
                onClick={() => revokeMut.mutate(node.node_pubkey)}
                disabled={revokeMut.isPending}
              >
                Revoke
              </Button>
            )}
          </div>
        ))}
      </div>

      <h3 className="text-lg font-semibold">Hardware Capacity</h3>
      {profiles.length === 0 ? (
        <p className="text-gray-400">No published hardware profiles</p>
      ) : (
        <div className="space-y-2">
          {profiles.map((p) => (
            <div
              key={fmtPubkey(p.node_pubkey)}
              className="rounded-lg border border-border bg-surface p-3 text-sm"
            >
              <p>
                {p.cpu.model}: {(p.available_memory_bytes / 1073741824).toFixed(1)}{" "}
                GiB available
              </p>
              <p className="text-xs text-gray-400">
                Backends:{" "}
                {p.runtimes.map((r) => r.backend).join(", ") || "none"}
              </p>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
