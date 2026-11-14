/**
 * Prefix a site-relative path with the configured base, normalising the
 * trailing slash of BASE_URL. `withBase("#top")` -> "/#top" or "/sub/#top".
 */
export function withBase(path = ""): string {
  const base = import.meta.env.BASE_URL.replace(/\/?$/, "/");
  return base + path.replace(/^\//, "");
}
