import { useState } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import {
  fetchStatus,
  drainPlacement,
  resumePlacement,
  previewPlacement,
} from "../lib/client";
import type { PlacementPreviewCandidate } from "../lib/types";
import { Button } from "../components/ui/button";
import { Input } from "../components/ui/input";

export function Models() {
  const queryClient = useQueryClient();
  const { data, isLoading, error } = useQuery({
    queryKey: ["status"],
    queryFn: fetchStatus,
    refetchInterval: 10_000,
  });

  const drainMut = useMutation({
    mutationFn: (id: string) => drainPlacement(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });

  const resumeMut = useMutation({
    mutationFn: (id: string) => resumePlacement(id),
    onSuccess: () => queryClient.invalidateQueries({ queryKey: ["status"] }),
  });

  if (isLoading) return <p className="text-gray-400">Loading models...</p>;
  if (error || !data) {
    return (
      <p className="text-unavailable">
        Models unavailable: {error?.message || "Unknown error"}
      </p>
    );
  }

  const modelMap = new Map<string, typeof data.placements>();
  for (const p of data.placements) {
    const group = modelMap.get(p.model) || [];
    group.push(p);
    modelMap.set(p.model, group);
  }

  return (
    <div className="space-y-6">
      <h2 className="text-xl font-semibold">Models &amp; Placements</h2>

      {modelMap.size === 0 ? (
        <p className="text-gray-400">No model placements</p>
      ) : (
        <div className="space-y-4">
          {[...modelMap.entries()].map(([model, placements]) => {
            const ready = placements.filter(
              (p) => p.health === "ready" && p.lifecycle === "ready",
            ).length;
            return (
              <div key={model} className="space-y-2">
                <p className="text-sm font-medium">
                  {model}: {ready}/{placements.length} replicas accepting work
                </p>
                {placements.map((p) => {
                  const draining = p.lifecycle === "draining";
                  return (
                    <div
                      key={p.placement_id}
                      className="flex items-center justify-between rounded border border-border bg-surface px-3 py-2 text-sm"
                    >
                      <span>
                        {p.placement_id}: {p.lifecycle}, {p.health}
                      </span>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() =>
                          draining
                            ? resumeMut.mutate(p.placement_id)
                            : drainMut.mutate(p.placement_id)
                        }
                        disabled={drainMut.isPending || resumeMut.isPending}
                      >
                        {draining ? "Resume" : "Drain"}
                      </Button>
                    </div>
                  );
                })}
              </div>
            );
          })}
        </div>
      )}

      <PlacementPreview />
    </div>
  );
}

function PlacementPreview() {
  const [model, setModel] = useState("");
  const [architecture, setArchitecture] = useState("");
  const [memory, setMemory] = useState("");
  const [backends, setBackends] = useState("cuda,vulkan,cpu");
  const [candidates, setCandidates] = useState<PlacementPreviewCandidate[]>();
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState("");

  const run = async () => {
    setErr("");
    setBusy(true);
    try {
      const mem = Number(memory);
      const backList = backends.split(",").map((s) => s.trim()).filter(Boolean);
      if (!model || !architecture || !Number.isSafeInteger(mem) || mem <= 0 || !backList.length) {
        throw new Error("Complete all placement preview fields");
      }
      const result = await previewPlacement({
        model,
        architecture,
        required_memory_bytes: mem,
        compatible_backends: backList,
      });
      setCandidates(result.candidates);
    } catch (e) {
      setErr(e instanceof Error ? e.message : "Preview failed");
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3 rounded-lg border border-border bg-surface p-4">
      <h3 className="text-sm font-semibold">Placement Preview</h3>
      <div className="grid grid-cols-2 gap-2">
        <Input placeholder="model ID" value={model} onChange={(e) => setModel(e.target.value)} />
        <Input placeholder="architecture (e.g. gemma3)" value={architecture} onChange={(e) => setArchitecture(e.target.value)} />
        <Input type="number" placeholder="required bytes" value={memory} onChange={(e) => setMemory(e.target.value)} />
        <Input placeholder="backends (comma-separated)" value={backends} onChange={(e) => setBackends(e.target.value)} />
      </div>
      <Button onClick={run} disabled={busy} size="sm">
        {busy ? "Running..." : "Preview"}
      </Button>
      {err && <p className="text-xs text-unavailable">{err}</p>}
      {candidates && (
        <div className="space-y-1 text-xs">
          {candidates.length === 0 ? (
            <p className="text-gray-400">No hardware profiles available</p>
          ) : (
            candidates.map((c, i) => (
              <p key={i}>
                {c.compatible ? "Compatible" : "Incompatible"}: {c.backend || "no backend"},{" "}
                {(c.memory_headroom_bytes / 1073741824).toFixed(1)} GiB headroom
                {c.decode_tokens_per_second_milli != null
                  ? `, ${(c.decode_tokens_per_second_milli / 1000).toFixed(2)} tok/s`
                  : ", unmeasured"}
                . {c.explanations.join("; ")}
              </p>
            ))
          )}
        </div>
      )}
    </div>
  );
}
