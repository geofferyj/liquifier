"use client";

import { useState, useCallback } from "react";
import { cn, shortenAddress } from "@/lib/utils";

interface CopyableAddressProps {
  address: string;
  shorten?: boolean;
  className?: string;
  label?: string;
}

export function CopyableAddress({
  address,
  shorten = true,
  className,
  label,
}: CopyableAddressProps) {
  const [showToast, setShowToast] = useState(false);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(address);
    } catch {
      // Fallback for older browsers
      const ta = document.createElement("textarea");
      ta.value = address;
      ta.style.position = "fixed";
      ta.style.opacity = "0";
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      document.body.removeChild(ta);
    }
    setShowToast(true);
    setTimeout(() => setShowToast(false), 1500);
  }, [address]);

  const display = shorten ? shortenAddress(address) : address;

  return (
    <>
      <span
        role="button"
        tabIndex={0}
        onClick={handleCopy}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") handleCopy();
        }}
        title={`Click to copy: ${address}`}
        className={cn(
          "font-mono cursor-pointer hover:text-primary transition-colors inline-flex items-center gap-1",
          className,
        )}
      >
        {label && <span className="font-sans">{label}</span>}
        {display}
        <svg
          xmlns="http://www.w3.org/2000/svg"
          viewBox="0 0 16 16"
          fill="currentColor"
          className="w-3 h-3 opacity-40 group-hover:opacity-70 shrink-0"
        >
          <path d="M5.5 3.5A1.5 1.5 0 0 1 7 2h5.5A1.5 1.5 0 0 1 14 3.5V9a1.5 1.5 0 0 1-1.5 1.5H7A1.5 1.5 0 0 1 5.5 9V3.5Z" />
          <path d="M3 5a1.5 1.5 0 0 0-1.5 1.5v6A1.5 1.5 0 0 0 3 14h6a1.5 1.5 0 0 0 1.5-1.5V11H7a3 3 0 0 1-3-3V5H3Z" />
        </svg>
      </span>

      {showToast && (
        <span
          className="fixed bottom-6 left-1/2 -translate-x-1/2 z-50 bg-foreground text-background px-4 py-2 rounded-md text-sm font-medium shadow-lg animate-in fade-in slide-in-from-bottom-2 duration-200"
        >
          Copied!
        </span>
      )}
    </>
  );
}
