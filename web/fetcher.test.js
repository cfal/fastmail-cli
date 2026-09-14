import assert from 'node:assert/strict';
import test from 'node:test';
import { parse } from 'graphql';
import { createFetcher } from './fetcher.js';

function sseResponse(body) {
  return new Response(body, { headers: { 'content-type': 'text/event-stream' } });
}

test('queries and mutations use the regular endpoint', async () => {
  const requests = [];
  const fetcher = createFetcher(async (url, options) => {
    requests.push({ url, options });
    return Response.json({ data: { __typename: 'QueryRoot' } });
  });
  for (const query of ['{ __typename }', 'mutation { markAsRead(emailId: "e0") { success } }']) {
    const result = await fetcher({ query }, { documentAST: parse(query) });
    if (result[Symbol.asyncIterator]) {
      for await (const value of result) assert.ok(value.data);
    } else {
      assert.ok(result.data);
    }
  }
  assert.deepEqual(requests.map(r => r.url), ['/graphql', '/graphql']);
});

test('subscriptions deliver streamed results and finish without reconnecting', async () => {
  let count = 0;
  const fetcher = createFetcher(async (url, options) => {
    count++;
    assert.equal(url, '/graphql/stream');
    assert.equal(options.method, 'POST');
    assert.equal(options.credentials, 'same-origin');
    assert.equal(new Headers(options.headers).get('authorization'), 'Basic test');
    assert.equal(JSON.parse(options.body).variables.id, 'e0');
    return sseResponse('event: next\ndata: {"data":{"emails":{"id":"e0"}}}\n\nevent: complete\ndata:\n\n');
  });
  const query = 'subscription Watch { emails { id } }';
  const values = [];
  for await (const value of fetcher({ query, operationName: 'Watch', variables: { id: 'e0' } }, { headers: { authorization: 'Basic test' } })) {
    values.push(value);
  }
  assert.deepEqual(values, [{ data: { emails: { id: 'e0' } } }]);
  assert.equal(count, 1);
});

test('routing uses the submitted query when editor analysis is stale', async () => {
  const requests = [];
  const fetcher = createFetcher(async (url) => {
    requests.push(url);
    if (url === '/graphql/stream') {
      return sseResponse('event: next\ndata: {"data":{"emails":{"id":"e0"}}}\n\nevent: complete\ndata:\n\n');
    }
    return Response.json({ data: { __typename: 'QueryRoot' } });
  });
  const query = '{ __typename }';
  const subscription = 'subscription { emails { id } }';
  for (const [submitted, stale] of [[subscription, query], [query, subscription]]) {
    const result = await fetcher({ query: submitted }, { documentAST: parse(stale) });
    if (result[Symbol.asyncIterator]) {
      for await (const value of result) assert.ok(value.data);
    } else {
      assert.ok(result.data);
    }
  }
  assert.deepEqual(requests, ['/graphql/stream', '/graphql']);
});

test('stopping a subscription aborts its request', async () => {
  let signal;
  const fetcher = createFetcher(async (_url, options) => {
    signal = options.signal;
    return sseResponse(new ReadableStream({ start(controller) {
      controller.enqueue(new TextEncoder().encode('event: next\ndata: {"data":{"emails":{"id":"e0"}}}\n\n'));
      signal.addEventListener('abort', () => controller.error(new DOMException('Aborted', 'AbortError')), { once: true });
    } }));
  });
  const stream = fetcher({ query: 'subscription { emails { id } }' });
  await stream.next();
  await stream.return();
  assert.equal(signal.aborted, true);
});

test('disconnects surface an error rather than silently skipping gap arrivals', async () => {
  let count = 0;
  const fetcher = createFetcher(async () => {
    count++;
    return sseResponse('');
  });
  const stream = fetcher({ query: 'subscription { emails { id } }' });
  await assert.rejects(stream.next());
  assert.equal(count, 1);
});

test('operationName selects the transport in a multi-operation document', async () => {
  const requests = [];
  const query = 'query Read { __typename } subscription Watch { emails { id } }';
  const fetcher = createFetcher(async (url, options) => {
    requests.push({ url, operationName: JSON.parse(options.body).operationName });
    if (url === '/graphql/stream') {
      return sseResponse('event: complete\ndata:\n\n');
    }
    return Response.json({ data: { __typename: 'QueryRoot' } });
  });
  for (const operationName of ['Watch', 'Read']) {
    const result = await fetcher({ query, operationName });
    if (result[Symbol.asyncIterator]) {
      for await (const value of result) assert.ok(value.data);
    } else {
      assert.ok(result.data);
    }
  }
  assert.deepEqual(requests, [
    { url: '/graphql/stream', operationName: 'Watch' },
    { url: '/graphql', operationName: 'Read' },
  ]);
});

test('each subscription uses its own request headers', async () => {
  const headers = [];
  const fetcher = createFetcher(async (_url, options) => {
    headers.push(new Headers(options.headers).get('authorization'));
    return sseResponse('event: complete\ndata:\n\n');
  });
  for (const authorization of ['Basic first', 'Basic second']) {
    const stream = fetcher({ query: 'subscription { emails { id } }' }, { headers: { authorization } });
    assert.deepEqual(await stream.next(), { value: undefined, done: true });
  }
  assert.deepEqual(headers, ['Basic first', 'Basic second']);
});

test('subscription authentication errors are surfaced without retrying', async () => {
  let requests = 0;
  const fetcher = createFetcher(async () => {
    requests++;
    return new Response('Authentication required', { status: 401 });
  });
  const stream = fetcher({ query: 'subscription { emails { id } }' });
  await assert.rejects(stream.next(), /401/);
  assert.equal(requests, 1);
});

test('invalid submitted syntax fails before fetching even with a valid stale AST', () => {
  const fetcher = createFetcher(() => assert.fail('Invalid syntax must not reach fetch'));
  assert.throws(
    () => fetcher({ query: '{' }, { documentAST: parse('{ __typename }') }),
    /Syntax Error/,
  );
});
