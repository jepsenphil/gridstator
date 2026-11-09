import { describe, expect, it } from 'vitest';
import { renderUnit, unitName } from './commands/service.js';

describe('systemd units', () => {
  it('renders a user unit for the controller', () => {
    const unit = renderUnit({
      role: 'controller',
      node: '/home/me/.nvm/versions/node/v24/bin/node',
      script: '/home/me/.nvm/versions/node/v24/lib/node_modules/gridstator/dist/gridstator.mjs',
      args: ['--with-agent', '--agent-name', 'my laptop'],
      pathDirs: ['/home/me/.local/bin', '/usr/bin', '/usr/bin'],
    });
    expect(unitName('controller')).toBe('gridstator-controller.service');
    expect(unit).toContain(
      'ExecStart=/home/me/.nvm/versions/node/v24/bin/node /home/me/.nvm/versions/node/v24/lib/node_modules/gridstator/dist/gridstator.mjs controller start --with-agent --agent-name "my laptop"',
    );
    expect(unit).toContain('Environment=PATH=/home/me/.local/bin:/usr/bin\n');
    expect(unit).toContain('WantedBy=default.target');
    expect(unit).not.toContain('User=');
  });

  it('renders a system unit for an agent that runs as the user with Docker access', () => {
    const unit = renderUnit({
      role: 'agent',
      node: '/usr/local/bin/node',
      script: '/usr/local/lib/node_modules/gridstator/dist/gridstator.mjs',
      args: ['--data-dir', '/home/pj/.local/share/gridstator/agent'],
      user: { name: 'pj', home: '/home/pj' },
      pathDirs: ['/usr/local/bin', '/usr/bin'],
    });
    expect(unit).toContain(
      'User=pj\nGroup=pj\nSupplementaryGroups=docker\nWorkingDirectory=/home/pj',
    );
    expect(unit).toContain('agent start --data-dir /home/pj/.local/share/gridstator/agent');
    expect(unit).toContain('WantedBy=multi-user.target');
  });
});
