import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { fetchCredentials, createCredential, revokeCredential } from "../lib/client";
import type { ManagementRole } from "../lib/types";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

export function Credentials() {
  const queryClient = useQueryClient();
  const { data, isLoading, error } = useQuery({
    queryKey: ["credentials"],
    queryFn: fetchCredentials,
  });

  const [label, setLabel] = useState("");
  const [role, setRole] = useState<ManagementRole>("viewer");
  const [createdToken, setCreatedToken] = useState("");

  const createMut = useMutation({
    mutationFn: () => createCredential(label, role),
    onSuccess: (result) => {
      setCreatedToken(result.token);
      queryClient.invalidateQueries({ queryKey: ["credentials"] });
    },
  });

  const revokeMut = useMutation({
    mutationFn: (id: string) => revokeCredential(id),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["credentials"] }),
  });

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Management Credentials</h2>

      {isLoading ? (
        <p className="text-gray-400">Loading credentials...</p>
      ) : error ? (
        <p className="text-unavailable">
          Credentials unavailable: {error.message}
        </p>
      ) : !data || data.length === 0 ? (
        <p className="text-gray-400">No management credentials</p>
      ) : (
        <div className="space-y-2">
          {data.map((cred) => (
            <div
              key={cred.id}
              className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
            >
              <span>
                {cred.label}: {cred.role},{" "}
                {cred.revocable ? "revocable" : "configured"}{" "}
                <span className="font-mono">({cred.id})</span>
              </span>
              {cred.revocable && (
                <Button
                  variant="destructive"
                  size="sm"
                  onClick={() => revokeMut.mutate(cred.id)}
                  disabled={revokeMut.isPending}
                >
                  Revoke
                </Button>
              )}
            </div>
          ))}
        </div>
      )}

      <div className="space-y-3 rounded-lg border border-border bg-surface p-4">
        <h3 className="text-sm font-semibold">Create Credential</h3>
        <div className="flex gap-2">
          <Input
            placeholder="credential label"
            value={label}
            onChange={(e) => setLabel(e.target.value)}
          />
          <select
            value={role}
            onChange={(e) => setRole(e.target.value as ManagementRole)}
            className="rounded-md border border-border bg-gray-950 px-3 py-1 text-sm"
          >
            <option value="viewer">viewer</option>
            <option value="operator">operator</option>
            <option value="admin">admin</option>
          </select>
          <Button
            size="sm"
            onClick={() => createMut.mutate()}
            disabled={createMut.isPending || !label.trim()}
          >
            Create
          </Button>
        </div>
        {createdToken && (
          <pre className="rounded bg-gray-950 p-3 text-xs text-ready">
            Save this token now. It will not be shown again.
            {"\n"}
            {createdToken}
          </pre>
        )}
      </div>
    </div>
  );
}
