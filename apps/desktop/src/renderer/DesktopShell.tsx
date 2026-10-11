import type { ReactNode } from 'react';

export type DesktopPage = 'tasks' | 'connection' | 'workspace' | 'observation';

const NAV_ITEMS: { page: DesktopPage; label: string; title: string; icon: 'compose' | 'plug' | 'summary' | 'activity' }[] = [
  { page: 'tasks', label: 'Tasks', title: '任务', icon: 'compose' },
  { page: 'connection', label: 'Model providers', title: '模型提供商', icon: 'plug' },
  { page: 'workspace', label: 'Workspace', title: '工作区', icon: 'summary' },
  { page: 'observation', label: 'Observation', title: '观察', icon: 'activity' },
];

export function DesktopShell({ activePage, onNavigate, toolbar, footer, children }: {
  activePage: DesktopPage;
  onNavigate(page: DesktopPage): void;
  toolbar: ReactNode;
  footer: ReactNode;
  children: ReactNode;
}) {
  return (
    <main className="desktop-navigation" data-workspace="true">
      <div className="drag-region desktop-window-toolbar">
        <div className="desktop-sidebar-brand">
          <strong>Computer Use</strong>
        </div>
        {toolbar}
      </div>
      <div className="desktop-body">
        <aside className="desktop-sidebar" aria-label="Computer Use navigation">
          <nav className="no-drag desktop-sidebar-list" aria-label="Views">
            {NAV_ITEMS.map(({ page, label, title, icon }) => (
              <button
                key={page}
                type="button"
                className="sidebar-nav-button"
                aria-current={activePage === page ? 'page' : undefined}
                title={title}
                onClick={() => onNavigate(page)}
              >
                <NavIcon name={icon} />
                <span>{label}</span>
              </button>
            ))}
          </nav>
          <footer className="sidebar-footer">{footer}</footer>
        </aside>
        <div className="desktop-surface">
          <div className="desktop-content">{children}</div>
        </div>
      </div>
    </main>
  );
}

function NavIcon({ name }: { name: (typeof NAV_ITEMS)[number]['icon'] }) {
  const common = {
    width: 18,
    height: 18,
    viewBox: '0 0 24 24',
    fill: 'none',
    stroke: 'currentColor',
    strokeWidth: 1.7,
    strokeLinecap: 'round' as const,
    strokeLinejoin: 'round' as const,
    'aria-hidden': true as const,
  };

  switch (name) {
    // These paths match LingXi Electron's compose, plug, summary, and activity
    // icons so the Computer Use shell uses the same quiet navigation language.
    case 'compose': return <svg {...common}><path d="M12 3H7a4 4 0 0 0-4 4v10a4 4 0 0 0 4 4h10a4 4 0 0 0 4-4v-5" /><path d="m10 14 1-4L18.5 2.5a2.12 2.12 0 0 1 3 3L14 13z" /></svg>;
    case 'plug': return <svg {...common}><path d="M9 2v6M15 2v6M7 8h10l-1 5a5 5 0 0 1-5 4 5 5 0 0 1-5-4z" /><path d="M12 19v3" /></svg>;
    case 'summary': return <svg {...common}><rect x="4" y="3" width="16" height="18" rx="3" /><path d="M8 8h8M8 12h8M8 16h5" /></svg>;
    case 'activity': return <svg {...common}><path d="M22 12h-4l-3 8-6-16-3 8H2" /></svg>;
  }
}
