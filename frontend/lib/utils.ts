import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

/** Format a raw U256 decimal string with given decimals to a human-readable amount */
export function formatTokenAmount(raw: string, decimals = 18, displayDecimals = 4): string {
  try {
    const value = BigInt(raw);
    const divisor = BigInt(10 ** decimals);
    const whole = value / divisor;
    const remainder = value % divisor;
    const fracStr = remainder.toString().padStart(decimals, "0").slice(0, displayDecimals);
    return `${whole.toLocaleString()}.${fracStr}`;
  } catch {
    return "0.0000";
  }
}

/** Calculate sell progress percentage */
export function sellProgress(amountSold: string, totalAmount: string): number {
  try {
    const sold  = BigInt(amountSold);
    const total = BigInt(totalAmount);
    if (total === 0n) return 0;
    return Number((sold * 10000n) / total) / 100;
  } catch {
    return 0;
  }
}

/** Shorten an Ethereum address: 0x1234…abcd */
export function shortenAddress(addr: string, chars = 4): string {
  if (!addr) return "";
  return `${addr.slice(0, chars + 2)}…${addr.slice(-chars)}`;
}

/** Convert basis points to percentage string */
export function bpsToPercent(bps: number): string {
  return `${(bps / 100).toFixed(2)}%`;
}
