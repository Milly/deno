// Copyright 2018-2024 the Deno authors. All rights reserved. MIT license.
import { core, internals } from "ext:core/mod.js";
const { internalRidSymbol } = core;
const { op_http_start } = core.ensureFastOps();

import { HttpConn } from "ext:deno_http/01_http.js";

function serveHttp(conn) {
  internals.warnOnDeprecatedApi(
    "Deno.serveHttp()",
    new Error().stack,
    "Use `Deno.serve()` instead.",
  );
  const rid = op_http_start(conn[internalRidSymbol]);
  return new HttpConn(rid, conn.remoteAddr, conn.localAddr);
}

export { serveHttp };
