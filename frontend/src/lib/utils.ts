import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function formatTokenAmount(raw: string, decimals: number): string {
  if (!raw || raw === "0") return "0";
  const padded = raw.padStart(decimals + 1, "0");
  const integer = padded.slice(0, padded.length - decimals) || "0";
  const fraction = padded.slice(padded.length - decimals).slice(0, 4);
  const intFormatted = integer.replace(/\B(?=(\d{3})+(?!\d))/g, ",");
  return `${intFormatted}.${fraction}`;
}

export function formatTokenAmountCompact(
  raw: string,
  decimals: number,
): string {
  if (!raw || raw === "0") return "0";

  try {
    const amountRaw = BigInt(raw);
    const divisor = 10n ** BigInt(decimals);
    const units = [
      { threshold: 1_000_000_000_000n, suffix: "T" },
      { threshold: 1_000_000_000n, suffix: "B" },
      { threshold: 1_000_000n, suffix: "M" },
      { threshold: 1_000n, suffix: "K" },
    ];

    for (const unit of units) {
      const unitDivisor = divisor * unit.threshold;
      if (amountRaw >= unitDivisor) {
        const scaledX100 = (amountRaw * 100n) / unitDivisor;
        const intPart = scaledX100 / 100n;
        const fracPart = scaledX100 % 100n;
        return `${intPart.toString()}.${fracPart.toString().padStart(2, "0")}${unit.suffix}`;
      }
    }
  } catch {
    // Fallback to full formatting below.
  }

  return formatTokenAmount(raw, decimals);
}

export function shortenAddress(addr: string): string {
  if (addr.length < 12) return addr;
  return `${addr.slice(0, 6)}...${addr.slice(-4)}`;
}

export function shortenTxHash(hash: string): string {
  if (hash.length < 16) return hash;
  return `${hash.slice(0, 10)}...${hash.slice(-6)}`;
}
