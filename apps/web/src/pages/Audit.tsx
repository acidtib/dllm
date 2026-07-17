import { useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { fetchAuditLog } from "../lib/client";

export function Audit() {
  const [limit] = useState(50);

  const { data, isLoading, error } = useQuery({
    queryKey: ["audit-log", limit],
    queryFn: () => fetchAuditLog(limit),
    refetchInterval: 15_000,
  });

  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold">Audit Log</h2>

      {isLoading ? (
        <p className="text-gray-400">Loading audit log...</p>
      ) : error ? (
        <p className="text-unavailable">
          Audit log unavailable: {error.message}
        </p>
      ) : !data || data.length === 0 ? (
        <p className="text-gray-400">No audit entries</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-sm">
            <thead>
              <tr className="border-b border-border text-xs text-gray-400">
                <th className="py-2 pr-4">Timestamp</th>
                <th className="py-2 pr-4">Actor</th>
                <th className="py-2 pr-4">Action</th>
                <th className="py-2 pr-4">Target</th>
                <th className="py-2">Outcome</th>
              </tr>
            </thead>
            <tbody>
              {data.map((entry, i) => (
                <tr key={i} className="border-b border-border text-xs">
                  <td className="py-2 pr-4 font-mono">
                    {entry.timestamp_unix || "?"}
                  </td>
                  <td className="py-2 pr-4">{entry.actor || ""}</td>
                  <td className="py-2 pr-4">{entry.action || ""}</td>
                  <td className="py-2 pr-4">{entry.target || ""}</td>
                  <td className="py-2">{entry.outcome || ""}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}
