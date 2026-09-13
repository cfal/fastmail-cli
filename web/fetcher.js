import { createGraphiQLFetcher } from '@graphiql/toolkit';
import { getOperationAST, parse } from 'graphql';
import { createClient } from 'graphql-sse';

export function createFetcher(fetchFn = globalThis.fetch) {
  const http = createGraphiQLFetcher({ url: '/graphql', fetch: fetchFn });
  return (params, options = {}) => {
    // GraphiQL's debounced editor analysis can lag behind the submitted query.
    const documentAST = parse(params.query);
    if (getOperationAST(documentAST, params.operationName)?.operation !== 'subscription') {
      return http(params, { ...options, documentAST });
    }
    const client = createClient({
      url: '/graphql/stream',
      singleConnection: false,
      credentials: 'same-origin',
      headers: options.headers,
      fetchFn,
      // A fresh subscription cannot replay mail received during a disconnect.
      retryAttempts: 0,
    });
    return client.iterate(params);
  };
}
