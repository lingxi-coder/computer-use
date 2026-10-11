import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';
import './styles.css';
import './electron-settings/provider-settings.css';

const container = document.getElementById('root');
if (!container) throw new Error('the page has no root element');
createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
