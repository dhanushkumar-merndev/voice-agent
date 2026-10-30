import { AlertCircleIcon, CircleCheckIcon, CircleDotIcon, CircleXIcon, ClockIcon, PhoneMissedIcon } from "lucide-react";

import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { inr } from "@/lib/format";
import { cn } from "@/lib/utils";
import type { CallStatus } from "@/lib/types";

export function PageHeader({
  title,
  description,
  actions,
}: {
  title: string;
  description?: React.ReactNode;
  actions?: React.ReactNode;
}) {
  return (
    <div className="mb-6 flex flex-wrap items-end justify-between gap-3">
      <div className="min-w-0">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="mt-1 text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}

const STATUS: Record<CallStatus, { label: string; icon: typeof CircleDotIcon; className: string }> = {
  queued: { label: "Queued", icon: ClockIcon, className: "text-muted-foreground" },
  ringing: { label: "Ringing", icon: CircleDotIcon, className: "text-blue-600 dark:text-blue-400" },
  in_progress: { label: "Live", icon: CircleDotIcon, className: "text-emerald-600 dark:text-emerald-400" },
  completed: { label: "Completed", icon: CircleCheckIcon, className: "text-foreground" },
  failed: { label: "Failed", icon: CircleXIcon, className: "text-destructive" },
  no_answer: { label: "No answer", icon: PhoneMissedIcon, className: "text-amber-600 dark:text-amber-400" },
  busy: { label: "Busy", icon: PhoneMissedIcon, className: "text-amber-600 dark:text-amber-400" },
};

/** Call status as icon + label (never colour alone). */
export function StatusBadge({ status, live }: { status: CallStatus; live?: boolean }) {
  const s = STATUS[status] ?? STATUS.queued;
  const Icon = s.icon;
  return (
    <Badge variant="outline" className={cn("gap-1 font-normal", s.className)}>
      <Icon className={cn("size-3", live && "animate-pulse")} />
      {s.label}
    </Badge>
  );
}

const INTEREST: Record<string, string> = {
  high: "bg-emerald-500/15 text-emerald-700 dark:text-emerald-300",
  medium: "bg-sky-500/15 text-sky-700 dark:text-sky-300",
  low: "bg-amber-500/15 text-amber-700 dark:text-amber-300",
  none: "bg-muted text-muted-foreground",
};

export function InterestBadge({ level }: { level: string | null | undefined }) {
  if (!level) return <span className="text-muted-foreground">—</span>;
  const label = level === "none" ? "Not interested" : `${level.charAt(0).toUpperCase()}${level.slice(1)} interest`;
  return (
    <Badge variant="secondary" className={cn("font-normal", INTEREST[level])}>
      {label}
    </Badge>
  );
}

/** A labelled number. `hero` is the one figure a view leads with. */
export function StatTile({
  label,
  value,
  sub,
  hero,
}: {
  label: string;
  value: React.ReactNode;
  sub?: React.ReactNode;
  hero?: boolean;
}) {
  return (
    <Card className="gap-1 py-4">
      <CardContent className="px-4">
        <p className="text-sm text-muted-foreground">{label}</p>
        <p className={cn("mt-1 font-semibold tabular-nums tracking-tight", hero ? "text-4xl md:text-5xl" : "text-2xl")}>
          {value}
        </p>
        {sub && <p className="mt-1 text-xs text-muted-foreground">{sub}</p>}
      </CardContent>
    </Card>
  );
}

/** A ₹ amount before GST; hover or focus shows it with GST. */
export function Money({
  excl,
  incl,
  gstPercent,
  className,
}: {
  excl: number | null | undefined;
  incl: number | null | undefined;
  gstPercent?: number;
  className?: string;
}) {
  if (excl === null || excl === undefined) return <span className={className}>—</span>;
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <span
          tabIndex={0}
          className={cn("cursor-help underline decoration-muted-foreground/40 decoration-dotted underline-offset-4", className)}
        >
          {inr(excl)}
        </span>
      </TooltipTrigger>
      <TooltipContent>
        {inr(incl)} incl. {gstPercent !== undefined ? `${gstPercent}% ` : ""}GST
      </TooltipContent>
    </Tooltip>
  );
}

export function ErrorAlert({ error, title = "Something went wrong" }: { error: unknown; title?: string }) {
  return (
    <Alert variant="destructive">
      <AlertCircleIcon />
      <AlertTitle>{title}</AlertTitle>
      <AlertDescription>{error instanceof Error ? error.message : String(error)}</AlertDescription>
    </Alert>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  children,
}: {
  icon: typeof CircleDotIcon;
  title: string;
  children?: React.ReactNode;
}) {
  return (
    <div className="flex flex-col items-center justify-center gap-2 rounded-xl border border-dashed p-10 text-center">
      <Icon className="size-8 text-muted-foreground" />
      <p className="font-medium">{title}</p>
      {children && <div className="text-sm text-muted-foreground">{children}</div>}
    </div>
  );
}

export function LoadingBlock({ rows = 3, className }: { rows?: number; className?: string }) {
  return (
    <div className={cn("grid gap-3", className)}>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-16 w-full" />
      ))}
    </div>
  );
}
