import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  fetchAccessRequests,
  approveAccessRequest,
  denyAccessRequest,
} from "../lib/client";
import { fmtPubkey } from "../lib/utils";
import { Button } from "../components/ui/button";

export function Access() {
  const queryClient = useQueryClient();
  const { data, isLoading, error } = useQuery({
    queryKey: ["access-requests"],
    queryFn: fetchAccessRequests,
    refetchInterval: 10_000,
  });

  const approveMut = useMutation({
    mutationFn: ({
      pubkey,
      endpoint,
    }: {
      pubkey: number[];
      endpoint?: string;
    }) => approveAccessRequest(pubkey, endpoint),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["access-requests"] }),
  });

  const denyMut = useMutation({
    mutationFn: (pubkey: number[]) => denyAccessRequest(pubkey),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["access-requests"] }),
  });

  if (isLoading) {
    return <p className="text-gray-400">Loading access requests...</p>;
  }
  if (error) {
    return (
      <p className="text-unavailable">
        Access requests unavailable: {error.message}
      </p>
    );
  }

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Access Requests</h2>

      {!data || data.length === 0 ? (
        <p className="text-gray-400">No pending access requests</p>
      ) : (
        <div className="space-y-2">
          {data.map((r) => {
            const req = "request" in r ? (r as { request: typeof r }).request : r;
            return (
              <div
                key={fmtPubkey(req.node_pubkey)}
                className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
              >
                <span>
                  <span className="font-mono">{fmtPubkey(req.node_pubkey)}...</span>{" "}
                  {req.requested_endpoint || "?"} {req.note || ""}
                </span>
                <div className="flex gap-2">
                  <Button
                    size="sm"
                    onClick={() =>
                      approveMut.mutate({
                        pubkey: req.node_pubkey,
                        endpoint: req.requested_endpoint,
                      })
                    }
                    disabled={approveMut.isPending}
                  >
                    Approve
                  </Button>
                  <Button
                    variant="destructive"
                    size="sm"
                    onClick={() => denyMut.mutate(req.node_pubkey)}
                    disabled={denyMut.isPending}
                  >
                    Deny
                  </Button>
                </div>
              </div>
            );
          })}
        </div>
      )}
    </div>
  );
}
