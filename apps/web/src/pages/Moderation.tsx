import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  fetchStatus,
  fetchAbuseReports,
  banNode,
  unbanNode,
  submitAbuseReport,
  setResourceBudget,
  removeResourceBudget,
} from "../lib/client";
import { fmtPubkey } from "../lib/utils";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

export function Moderation() {
  const { data, isLoading, error } = useQuery({
    queryKey: ["status"],
    queryFn: fetchStatus,
    refetchInterval: 10_000,
  });

  const { data: abuseReports } = useQuery({
    queryKey: ["abuse-reports"],
    queryFn: fetchAbuseReports,
    refetchInterval: 10_000,
  });

  if (isLoading) return <p className="text-gray-400">Loading moderation...</p>;
  if (error || !data) {
    return (
      <p className="text-unavailable">
        Moderation unavailable: {error?.message || "Unknown error"}
      </p>
    );
  }

  const network = data.network.state;
  const bans = network.banned || [];
  const budgets = network.resource_budgets || [];

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Moderation</h2>

      {/* Bans */}
      <h3 className="text-lg font-semibold">Banned Nodes</h3>
      {bans.length === 0 ? (
        <p className="text-gray-400">No banned nodes</p>
      ) : (
        <div className="space-y-2">
          {bans.map((b) => (
            <div
              key={fmtPubkey(b.node_pubkey)}
              className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
            >
              <span>
                <span className="font-mono">{fmtPubkey(b.node_pubkey)}...</span>{" "}
                reason: {b.reason} (banned at {b.banned_at_unix})
              </span>
              <UnbanButton pubkey={b.node_pubkey} />
            </div>
          ))}
        </div>
      )}

      <BanForm />

      {/* Resource Budgets */}
      <h3 className="text-lg font-semibold">Resource Budgets</h3>
      {budgets.length === 0 ? (
        <p className="text-gray-400">No resource budgets</p>
      ) : (
        <div className="space-y-2">
          {budgets.map((b) => (
            <div
              key={fmtPubkey(b.node_pubkey)}
              className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
            >
              <span>
                <span className="font-mono">{fmtPubkey(b.node_pubkey)}...</span>{" "}
                in-flight: {b.max_in_flight}, window: {b.max_requests_per_window}/
                {b.window_seconds}s
              </span>
              <RemoveBudgetButton pubkey={b.node_pubkey} />
            </div>
          ))}
        </div>
      )}

      <BudgetForm />

      {/* Abuse Reports */}
      <h3 className="text-lg font-semibold">Abuse Reports</h3>
      {!abuseReports || abuseReports.length === 0 ? (
        <p className="text-gray-400">No abuse reports</p>
      ) : (
        <div className="space-y-2">
          {abuseReports.map((r, i) => (
            <div
              key={i}
              className="rounded border border-border bg-surface px-3 py-2 text-xs"
            >
              <p>
                reporter{" "}
                <span className="font-mono">{fmtPubkey(r.reporter_pubkey)}</span>{" "}
                subject{" "}
                <span className="font-mono">{fmtPubkey(r.subject_pubkey)}</span>{" "}
                {r.category}: {r.note || ""}
              </p>
            </div>
          ))}
        </div>
      )}

      <AbuseForm />
    </div>
  );
}

function UnbanButton({ pubkey }: { pubkey: number[] }) {
  const queryClient = useQueryClient();
  const mut = useMutation({
    mutationFn: () => unbanNode(pubkey),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });
  return (
    <Button
      variant="destructive"
      size="sm"
      onClick={() => mut.mutate()}
      disabled={mut.isPending}
    >
      Unban
    </Button>
  );
}

function BanForm() {
  const queryClient = useQueryClient();
  const [hex, setHex] = useState("");
  const [reason, setReason] = useState("");
  const mut = useMutation({
    mutationFn: () => banNode(hexToBytes(hex), reason),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });
  return (
    <div className="flex gap-2">
      <Input placeholder="node key (hex)" value={hex} onChange={(e) => setHex(e.target.value)} />
      <Input placeholder="reason" value={reason} onChange={(e) => setReason(e.target.value)} />
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Ban
      </Button>
    </div>
  );
}

function BudgetForm() {
  const queryClient = useQueryClient();
  const [hex, setHex] = useState("");
  const [inflight, setInflight] = useState("");
  const [perWindow, setPerWindow] = useState("");
  const [windowSec, setWindowSec] = useState("");
  const mut = useMutation({
    mutationFn: () =>
      setResourceBudget(
        hexToBytes(hex),
        Number(inflight),
        Number(perWindow),
        Number(windowSec),
      ),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });
  return (
    <div className="flex gap-2">
      <Input placeholder="node key (hex)" value={hex} onChange={(e) => setHex(e.target.value)} />
      <Input placeholder="max in flight" type="number" value={inflight} onChange={(e) => setInflight(e.target.value)} />
      <Input placeholder="max per window" type="number" value={perWindow} onChange={(e) => setPerWindow(e.target.value)} />
      <Input placeholder="window seconds" type="number" value={windowSec} onChange={(e) => setWindowSec(e.target.value)} />
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Set
      </Button>
    </div>
  );
}

function RemoveBudgetButton({ pubkey }: { pubkey: number[] }) {
  const queryClient = useQueryClient();
  const mut = useMutation({
    mutationFn: () => removeResourceBudget(pubkey),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });
  return (
    <Button
      variant="destructive"
      size="sm"
      onClick={() => mut.mutate()}
      disabled={mut.isPending}
    >
      Remove
    </Button>
  );
}

function AbuseForm() {
  const queryClient = useQueryClient();
  const [subjectHex, setSubjectHex] = useState("");
  const [category, setCategory] = useState("");
  const [note, setNote] = useState("");
  const mut = useMutation({
    mutationFn: () =>
      submitAbuseReport({
        subject_pubkey: hexToBytes(subjectHex),
        category,
        note,
      }),
    onSuccess: () =>
      queryClient.invalidateQueries({ queryKey: ["abuse-reports"] }),
  });
  return (
    <div className="flex gap-2">
      <Input placeholder="subject pubkey (hex)" value={subjectHex} onChange={(e) => setSubjectHex(e.target.value)} />
      <Input placeholder="category" value={category} onChange={(e) => setCategory(e.target.value)} />
      <Input placeholder="note" value={note} onChange={(e) => setNote(e.target.value)} />
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Submit
      </Button>
    </div>
  );
}

function hexToBytes(hex: string): number[] {
  const s = hex.replace(/\s/g, "");
  if (s.length % 2 !== 0) throw new Error("hex string must have even length");
  const bytes: number[] = [];
  for (let i = 0; i < s.length; i += 2) {
    bytes.push(parseInt(s.substring(i, i + 2), 16));
  }
  if (bytes.length !== 32) throw new Error("node pubkey must be 32 bytes");
  return bytes;
}
