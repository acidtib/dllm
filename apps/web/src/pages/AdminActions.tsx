import { useState } from "react";
import { useMutation } from "@tanstack/react-query";
import { createInvitation, assignModel } from "../lib/client";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

export function InvitationSection() {
  const [result, setResult] = useState("");

  const mut = useMutation({
    mutationFn: () => createInvitation(null),
    onSuccess: (token) => {
      setResult(JSON.stringify(token, null, 2));
    },
  });

  return (
    <div className="space-y-3 rounded-lg border border-border bg-surface p-4">
      <h3 className="text-sm font-semibold">Invitation</h3>
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Generate Invitation
      </Button>
      {result && (
        <pre className="rounded bg-gray-950 p-3 text-xs text-ready">
          {result}
        </pre>
      )}
    </div>
  );
}

export function AssignModelSection({
  authorityPubkey,
}: {
  authorityPubkey: number[];
}) {
  const [model, setModel] = useState("");
  const [msg, setMsg] = useState("");
  const [busy, setBusy] = useState(false);

  const assign = async () => {
    setMsg("");
    setBusy(true);
    try {
      await assignModel(model.trim(), authorityPubkey);
      setMsg("Model assigned");
      setModel("");
    } catch (e) {
      setMsg(e instanceof Error ? e.message : "Assignment failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3 rounded-lg border border-border bg-surface p-4">
      <h3 className="text-sm font-semibold">Assign Authority Model</h3>
      <div className="flex gap-2">
        <Input
          placeholder="model ID"
          value={model}
          onChange={(e) => setModel(e.target.value)}
        />
        <Button size="sm" onClick={assign} disabled={busy || !model.trim()}>
          Assign
        </Button>
      </div>
      {msg && <p className="text-xs text-gray-400">{msg}</p>}
    </div>
  );
}

export function RecoveryNote() {
  return (
    <div className="rounded-lg border border-border bg-surface p-4">
      <h3 className="text-sm font-semibold">Recovery</h3>
      <p className="text-xs text-gray-400">
        Encrypted backup, restore, and authority transfer are offline CLI operations
        so authority keys are never sent to this page.
      </p>
    </div>
  );
}
