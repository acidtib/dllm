import { useState } from "react";
import { BrowserRouter, Routes, Route, NavLink } from "react-router-dom";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Toaster } from "sonner";
import { setToken } from "./lib/api";
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

const NAV_ITEMS = [
  { to: "/", label: "Overview" },
  { to: "/nodes", label: "Nodes" },
  { to: "/models", label: "Models" },
  { to: "/playground", label: "Playground" },
  { to: "/peers", label: "Peers" },
  { to: "/access", label: "Access" },
  { to: "/credentials", label: "Credentials" },
  { to: "/moderation", label: "Moderation" },
  { to: "/audit", label: "Audit" },
];

function Layout({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex min-h-screen">
      <nav className="w-56 shrink-0 border-r border-border bg-surface p-4">
        <h1 className="mb-4 text-lg font-semibold tracking-tight">
          <NavLink to="/">DLLM</NavLink>
        </h1>
        <ul className="space-y-1">
          {NAV_ITEMS.map((item) => (
            <li key={item.to}>
              <NavLink
                to={item.to}
                className={({ isActive }) =>
                  `block rounded px-3 py-1.5 text-sm transition-colors ${
                    isActive
                      ? "bg-accent text-gray-950 font-medium"
                      : "text-gray-400 hover:bg-surface-hover hover:text-gray-200"
                  }`
                }
              >
                {item.label}
              </NavLink>
            </li>
          ))}
        </ul>
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
