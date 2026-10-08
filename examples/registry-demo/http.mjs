// HTTP and CORS policy lives outside the no_std router. This is a public,
// read-only demo, so every origin may read it, including credentialed fetches.
export function registryFetch(request, dispatch) {
  const url = new URL(request.url);
  const headers = new Headers({
    "Docker-Distribution-API-Version": "registry/2.0",
    "Vary": "Origin",
  });
  const origin = request.headers.get("Origin");
  if (origin) {
    headers.set("Access-Control-Allow-Origin", origin);
    headers.set("Access-Control-Allow-Credentials", "true");
    headers.set("Access-Control-Allow-Methods", "GET, HEAD, OPTIONS");
    headers.set("Access-Control-Allow-Headers", "Accept, Content-Type, Range");
    headers.set("Access-Control-Expose-Headers", "Docker-Content-Digest, Docker-Distribution-API-Version, Content-Length, Link");
    headers.set("Access-Control-Max-Age", "86400");
  }
  let result;
  try {
    result = dispatch(request.method, url.pathname + url.search);
    headers.set("Content-Type", result.media_type);
    headers.set("Content-Length", String(result.length));
    if (result.digest) headers.set("Docker-Content-Digest", result.digest);
    if (result.link) headers.set("Link", result.link);
    if (result.status === 405) headers.set("Allow", "GET, HEAD, OPTIONS");
    return new Response(request.method === "HEAD" || result.status === 204 ? null : result.body, {
      status: result.status, headers,
    });
  } catch {
    return new Response("registry response failed\n", { status: 500, headers });
  } finally {
    result?.free();
  }
}
