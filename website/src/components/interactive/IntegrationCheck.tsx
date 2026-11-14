import { useState } from "react";

/**
 * Minimal island that proves the React integration works. It is not used on
 * any page, so no React JavaScript ships until an island is mounted with a
 * `client:*` directive. Replace with real interactive components later.
 */
export default function IntegrationCheck() {
  const [count, setCount] = useState(0);
  return (
    <button
      type="button"
      className="rounded-control border border-border bg-surface px-3 py-1.5 text-sm text-text"
      onClick={() => setCount((value) => value + 1)}
    >
      React is hydrated · {count}
    </button>
  );
}
