import { useState } from "react";
import { BrowserRouter, Routes, Route, NavLink } from "react-router-dom";
import { QueryClient, QueryClientProvider, useQuery } from "@tanstack/react-query";
import { Toaster } from "sonner";
import {
  Box,
  Circle,
  KeyRound,
  LayoutDashboard,
  LogOut,
  Menu,
  MessageSquare,
  Network,
  ScrollText,
  Server,
  ShieldAlert,
  UserCheck,
  type LucideIcon,
} from "lucide-react";
import { clearToken, setToken } from "./lib/api";
import { fetchStatus } from "./lib/client";
import { cn, healthClass } from "./lib/utils";
import { Overview } from "./pages/Overview";
import { Nodes } from "./pages/Nodes";
import { Models } from "./pages/Models";
import { Playground } from "./pages/Playground";
import { Peers } from "./pages/Peers";
import { Access } from "./pages/Access";
import { Credentials } from "./pages/Credentials";
import { Moderation } from "./pages/Moderation";
import { Audit } from "./pages/Audit";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      staleTime: 5_000,
    },
  },
});

function TokenGate({ children }: { children: React.ReactNode }) {
  const [token, setTokenState] = useState(
    () => localStorage.getItem("dllm-management-token") || "",
  );

  if (!token) {
    return (
      <div className="flex min-h-screen items-center justify-center">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            const form = e.currentTarget;
            const input = form.elements.namedItem("token") as HTMLInputElement;
            const value = input.value.trim();
            if (!value) return;
            setToken(value);
            setTokenState(value);
          }}
          className="w-full max-w-sm space-y-4 rounded-lg border border-border bg-surface p-6"
        >
          <h1 className="text-xl font-semibold">DLLM Management</h1>
          <p className="text-sm text-gray-400">
            Enter a management token to access this dashboard.
          </p>
          <input
            name="token"
            type="password"
            autoComplete="off"
            placeholder="Management token"
            className="w-full rounded border border-border bg-gray-950 px-3 py-2 text-sm"
          />
          <button
            type="submit"
            className="w-full rounded bg-accent px-4 py-2 text-sm font-medium text-gray-950 hover:bg-accent-hover"
          >
            Authenticate
          </button>
        </form>
      </div>
    );
  }

  return <>{children}</>;
}

interface NavItem {
  to: string;
  label: string;
  icon: LucideIcon;
}

const NAV_GROUPS: { label: string; items: NavItem[] }[] = [
  {
    label: "Monitor",
    items: [
      { to: "/", label: "Overview", icon: LayoutDashboard },
      { to: "/nodes", label: "Nodes", icon: Server },
      { to: "/models", label: "Models", icon: Box },
      { to: "/peers", label: "Peers", icon: Network },
      { to: "/audit", label: "Audit", icon: ScrollText },
    ],
  },
  {
    label: "Use",
    items: [{ to: "/playground", label: "Playground", icon: MessageSquare }],
  },
  {
    label: "Admin",
    items: [
      { to: "/access", label: "Access", icon: UserCheck },
      { to: "/credentials", label: "Credentials", icon: KeyRound },
      { to: "/moderation", label: "Moderation", icon: ShieldAlert },
    ],
  },
];

function HealthIndicator() {
  const { data, error } = useQuery({
    queryKey: ["status"],
    queryFn: fetchStatus,
    refetchInterval: 10_000,
  });

  const health = error ? "unavailable" : data?.health || "unknown";

  return (
    <div className="flex items-center gap-2 rounded-md border border-border bg-gray-950 px-3 py-2 text-xs">
      <Circle className={cn("h-2.5 w-2.5 fill-current", healthClass(health))} />
      <span className={healthClass(health)}>{health}</span>
    </div>
  );
}

function NavContents({ onNavigate }: { onNavigate?: () => void }) {
  return (
    <>
      <h1 className="mb-4 text-lg font-semibold tracking-tight">
        <NavLink to="/" onClick={onNavigate}>
          DLLM
        </NavLink>
      </h1>

      <HealthIndicator />

      <div className="mt-4 flex-1 space-y-4">
        {NAV_GROUPS.map((group) => (
          <div key={group.label}>
            <p className="mb-1 px-3 text-xs font-medium uppercase tracking-wide text-gray-500">
              {group.label}
            </p>
            <ul className="space-y-1">
              {group.items.map((item) => (
                <li key={item.to}>
                  <NavLink
                    to={item.to}
                    end={item.to === "/"}
                    onClick={onNavigate}
                    className={({ isActive }) =>
                      cn(
                        "flex items-center gap-2 rounded px-3 py-1.5 text-sm transition-colors",
                        isActive
                          ? "bg-accent font-medium text-gray-950"
                          : "text-gray-400 hover:bg-surface-hover hover:text-gray-200",
                      )
                    }
                  >
                    <item.icon className="h-4 w-4 shrink-0" />
                    {item.label}
                  </NavLink>
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>

      <button
        type="button"
        onClick={() => {
          clearToken();
          location.reload();
        }}
        className="flex items-center gap-2 rounded px-3 py-1.5 text-sm text-gray-400 hover:bg-surface-hover hover:text-gray-200"
      >
        <LogOut className="h-4 w-4 shrink-0" />
        Log out
      </button>
    </>
  );
}

function Layout({ children }: { children: React.ReactNode }) {
  const [open, setOpen] = useState(false);

  return (
    <div className="flex min-h-screen flex-col md:flex-row">
      <header className="flex items-center justify-between border-b border-border bg-surface p-4 md:hidden">
        <span className="text-lg font-semibold tracking-tight">DLLM</span>
        <button
          type="button"
          onClick={() => setOpen((o) => !o)}
          className="rounded p-1.5 hover:bg-surface-hover"
          aria-label="Toggle navigation"
        >
          <Menu className="h-5 w-5" />
        </button>
      </header>

      <nav
        className={cn(
          "w-full shrink-0 flex-col border-r border-border bg-surface p-4 md:flex md:w-56",
          open ? "flex" : "hidden",
        )}
      >
        <NavContents onNavigate={() => setOpen(false)} />
      </nav>

      <main className="flex-1 p-6">{children}</main>
    </div>
  );
}

export function App() {
  return (
    <QueryClientProvider client={queryClient}>
      <BrowserRouter>
        <TokenGate>
          <Layout>
            <Routes>
              <Route path="/" element={<Overview />} />
              <Route path="/nodes" element={<Nodes />} />
              <Route path="/models" element={<Models />} />
              <Route path="/playground" element={<Playground />} />
              <Route path="/peers" element={<Peers />} />
              <Route path="/access" element={<Access />} />
              <Route path="/credentials" element={<Credentials />} />
              <Route path="/moderation" element={<Moderation />} />
              <Route path="/audit" element={<Audit />} />
            </Routes>
          </Layout>
        </TokenGate>
      </BrowserRouter>
      <Toaster
        toastOptions={{
          style: {
            background: "#1f2937",
            color: "#e5e7eb",
            border: "1px solid #374151",
          },
        }}
      />
    </QueryClientProvider>
  );
}
