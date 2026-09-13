import React from 'react';
import { createRoot } from 'react-dom/client';
import { GraphiQL, HISTORY_PLUGIN } from 'graphiql';
import { createGraphiQLFetcher } from '@graphiql/toolkit';
import { explorerPlugin } from '@graphiql/plugin-explorer';
import 'graphiql/style.css';
import '@graphiql/plugin-explorer/style.css';
import './style.css';

globalThis.MonacoEnvironment = {
  getWorker(_id, label) {
    const name = label === 'graphql' || label === 'json' ? label : 'editor';
    return new Worker(`/assets/${name}.worker.js`, { type: 'module' });
  },
};

createRoot(document.getElementById('graphiql')).render(
  React.createElement(GraphiQL, {
    fetcher: createGraphiQLFetcher({ url: '/graphql' }),
    plugins: [HISTORY_PLUGIN, explorerPlugin()],
    defaultEditorToolsVisibility: true,
    defaultQuery: '{ __typename }',
    storage: null,
    shouldPersistHeaders: false,
  }),
);
