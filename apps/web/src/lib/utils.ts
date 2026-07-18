import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function fmtPubkey(bytes: number[]): string {
  return bytes
    .slice(0, 4)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

export function bytesToHex(bytes: number[]): string {
  return bytes.map((b) => b.toString(16).padStart(2, "0")).join("");
}

export function hexToBytes(hex: string): number[] {
  const s = hex.replace(/\s/g, "");
  if (s.length % 2 !== 0) throw new Error("hex string must have even length");
  const bytes: number[] = [];
  for (let i = 0; i < s.length; i += 2) {
    bytes.push(parseInt(s.substring(i, i + 2), 16));
  }
  if (bytes.length !== 32) throw new Error("node pubkey must be 32 bytes");
  return bytes;
}

export function fmtBytes(n: number): string {
  const gib = n / 1073741824;
  return `${gib.toFixed(1)} GiB`;
}

export function fmtUnix(ts: number): string {
  if (!ts) return "?";
  return new Date(ts * 1000).toLocaleString();
}

const HEALTH_CLASS: Record<string, string> = {
  ready: "text-ready",
  degraded: "text-degraded",
  unavailable: "text-unavailable",
  unknown: "text-gray-400",
};

export function healthClass(health: string): string {
  return HEALTH_CLASS[health] ?? "text-gray-400";
}
