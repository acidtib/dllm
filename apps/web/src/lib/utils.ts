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

export function fmtBytes(n: number): string {
  const gib = n / 1073741824;
  return `${gib.toFixed(1)} GiB`;
}

export function fmtUnix(ts: number): string {
  return new Date(ts * 1000).toLocaleString();
}
