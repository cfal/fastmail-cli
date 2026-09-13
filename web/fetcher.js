import { createGraphiQLFetcher } from '@graphiql/toolkit';
import { getOperationAST, parse } from 'graphql';
import { createClient } from 'graphql-sse';

export function createFetcher(fetchFn = globalThis.fetch) {
  const http = createGraphiQLFetcher({ url: '/graphql', fetch: fetchFn });
  return (params, options = {}) => {
    const document = options.documentAST || parse(params.query);
    if (getOperationAST(document, params.operationName)?.operation !== 'subscription') {
      return http(params, options);
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
