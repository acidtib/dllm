import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import {
  fetchStatus,
  fetchAbuseReports,
  banNode,
  unbanNode,
  submitAbuseReport,
  setResourceBudget,
  removeResourceBudget,
} from "../lib/client";
import type { NodeStatus } from "../lib/types";
import { bytesToHex, fmtPubkey, fmtUnix, hexToBytes } from "../lib/utils";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";
import { PubkeyBadge } from "../components/ui/pubkey-badge";

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
                <PubkeyBadge bytes={b.node_pubkey} /> reason: {b.reason}{" "}
                (banned {fmtUnix(b.banned_at_unix)})
              </span>
              <UnbanButton pubkey={b.node_pubkey} />
            </div>
          ))}
        </div>
      )}

      <BanForm nodes={data.nodes} />

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
                <PubkeyBadge bytes={b.node_pubkey} /> in-flight:{" "}
                {b.max_in_flight}, window: {b.max_requests_per_window}/
                {b.window_seconds}s
              </span>
              <RemoveBudgetButton pubkey={b.node_pubkey} />
            </div>
          ))}
        </div>
      )}

      <BudgetForm nodes={data.nodes} />

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
                reporter <PubkeyBadge bytes={r.reporter_pubkey} /> subject{" "}
                <PubkeyBadge bytes={r.subject_pubkey} /> {r.category}:{" "}
                {r.note || ""}
              </p>
            </div>
          ))}
        </div>
      )}

      <AbuseForm nodes={data.nodes} />
    </div>
  );
}

function UnbanButton({ pubkey }: { pubkey: number[] }) {
  const queryClient = useQueryClient();
  const mut = useMutation({
    mutationFn: () => unbanNode(pubkey),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["status"] });
      toast.success("Node unbanned");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Unban failed"),
  });
  return (
    <Button
      variant="destructive"
      size="sm"
      onClick={() => {
        if (!window.confirm(`Unban node ${fmtPubkey(pubkey)}...?`)) return;
        mut.mutate();
      }}
      disabled={mut.isPending}
    >
      Unban
    </Button>
  );
}

function NodePicker({
  nodes,
  onPick,
}: {
  nodes: NodeStatus[];
  onPick: (hex: string) => void;
}) {
  return (
    <select
      value=""
      onChange={(e) => e.target.value && onPick(e.target.value)}
      className="rounded-md border border-border bg-gray-950 px-3 py-1.5 text-sm"
    >
      <option value="">Pick a known node&hellip;</option>
      {nodes.map((n) => (
        <option key={bytesToHex(n.node_pubkey)} value={bytesToHex(n.node_pubkey)}>
          {fmtPubkey(n.node_pubkey)}... {n.endpoint}
        </option>
      ))}
    </select>
  );
}

function BanForm({ nodes }: { nodes: NodeStatus[] }) {
  const queryClient = useQueryClient();
  const [hex, setHex] = useState("");
  const [reason, setReason] = useState("");
  const mut = useMutation({
    mutationFn: () => banNode(hexToBytes(hex), reason),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["status"] });
      toast.success("Node banned");
      setHex("");
      setReason("");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Ban failed"),
  });
  return (
    <div className="flex flex-wrap gap-2">
      <NodePicker nodes={nodes} onPick={setHex} />
      <Input placeholder="node key (hex)" value={hex} onChange={(e) => setHex(e.target.value)} className="min-w-[220px] flex-1" />
      <Input placeholder="reason" value={reason} onChange={(e) => setReason(e.target.value)} />
      <Button
        size="sm"
        onClick={() => {
          if (!window.confirm(`Ban node ${hex.slice(0, 8)}...?`)) return;
          mut.mutate();
        }}
        disabled={mut.isPending}
      >
        Ban
      </Button>
    </div>
  );
}

function BudgetForm({ nodes }: { nodes: NodeStatus[] }) {
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
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["status"] });
      toast.success("Resource budget set");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Set failed"),
  });
  return (
    <div className="flex flex-wrap gap-2">
      <NodePicker nodes={nodes} onPick={setHex} />
      <Input placeholder="node key (hex)" value={hex} onChange={(e) => setHex(e.target.value)} className="min-w-[220px] flex-1" />
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
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["status"] });
      toast.success("Resource budget removed");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Remove failed"),
  });
  return (
    <Button
      variant="destructive"
      size="sm"
      onClick={() => {
        if (!window.confirm(`Remove resource budget for ${fmtPubkey(pubkey)}...?`)) return;
        mut.mutate();
      }}
      disabled={mut.isPending}
    >
      Remove
    </Button>
  );
}

function AbuseForm({ nodes }: { nodes: NodeStatus[] }) {
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
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ["abuse-reports"] });
      toast.success("Abuse report submitted");
      setSubjectHex("");
      setCategory("");
      setNote("");
    },
    onError: (e) => toast.error(e instanceof Error ? e.message : "Submit failed"),
  });
  return (
    <div className="flex flex-wrap gap-2">
      <NodePicker nodes={nodes} onPick={setSubjectHex} />
      <Input placeholder="subject pubkey (hex)" value={subjectHex} onChange={(e) => setSubjectHex(e.target.value)} className="min-w-[220px] flex-1" />
      <Input placeholder="category" value={category} onChange={(e) => setCategory(e.target.value)} />
      <Input placeholder="note" value={note} onChange={(e) => setNote(e.target.value)} />
      <Button size="sm" onClick={() => mut.mutate()} disabled={mut.isPending}>
        Submit
      </Button>
    </div>
  );
}
