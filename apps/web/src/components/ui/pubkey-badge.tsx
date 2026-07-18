import { toast } from "sonner";
import { bytesToHex, cn, fmtPubkey } from "../../lib/utils";

export function PubkeyBadge({
  bytes,
  className,
}: {
  bytes: number[];
  className?: string;
}) {
  const full = bytesToHex(bytes);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(full);
      toast.success("Pubkey copied to clipboard");
    } catch {
      toast.error("Could not copy to clipboard");
    }
  };

  return (
    <button
      type="button"
      onClick={copy}
      title={`${full} (click to copy)`}
      className={cn(
        "font-mono underline decoration-dotted decoration-gray-600 underline-offset-2 hover:text-accent",
        className,
      )}
    >
      {fmtPubkey(bytes)}&hellip;
    </button>
  );
}
