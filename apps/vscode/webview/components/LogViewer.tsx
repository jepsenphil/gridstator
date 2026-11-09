import { useEffect, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import type { JobLogs, LogLine } from '../store';
import { cx, send, stripAnsi } from '../util';
import { Button, Icon } from './common';

const LINE_HEIGHT = 18;
const OVERSCAN = 30;
const WRAP_LIMIT = 4000;
const PATH =
  /((?:\/workspace\/|\/[^\s:()'"]*\/work\/[0-9A-Z]{26}\/src\/)[^\s:()'"]+)(?::(\d+))?(?::(\d+))?/g;

function renderLine(text: string, jobId: string, query: string): ReactNode[] {
  const out: ReactNode[] = [];
  let last = 0;
  for (const m of text.matchAll(PATH)) {
    const idx = m.index ?? 0;
    if (idx > last) out.push(...highlight(text.slice(last, idx), query, out.length));
    const [whole, path, line, col] = m;
    out.push(
      <a
        key={`p${idx}`}
        className="log-link"
        title="Open in editor"
        onClick={() =>
          send({
            type: 'openFile',
            jobId,
            path: path!,
            ...(line ? { line: Number(line) } : {}),
            ...(col ? { column: Number(col) } : {}),
          })
        }
      >
        {whole}
      </a>,
    );
    last = idx + whole.length;
  }
  if (last < text.length) out.push(...highlight(text.slice(last), query, out.length));
  return out;
}

function highlight(text: string, query: string, keyBase: number): ReactNode[] {
  if (!query) return [text];
  const lower = text.toLowerCase();
  const q = query.toLowerCase();
  const out: ReactNode[] = [];
  let i = 0;
  let k = 0;
  for (;;) {
    const j = lower.indexOf(q, i);
    if (j === -1) break;
    if (j > i) out.push(text.slice(i, j));
    out.push(<mark key={`h${keyBase}-${k++}`}>{text.slice(j, j + q.length)}</mark>);
    i = j + q.length;
  }
  out.push(text.slice(i));
  return out;
}

export function LogViewer({
  jobId,
  logs,
  active,
}: {
  jobId: string;
  logs: JobLogs | undefined;
  active: boolean;
}) {
  const [query, setQuery] = useState('');
  const [streams, setStreams] = useState({ stdout: true, stderr: true, system: true });
  const [follow, setFollow] = useState(true);
  const [wrap, setWrap] = useState(false);
  const [scrollTop, setScrollTop] = useState(0);
  const [height, setHeight] = useState(400);
  const box = useRef<HTMLDivElement>(null);
  const lines = useMemo(() => {
    const all = logs?.lines ?? [];
    const q = query.toLowerCase();
    return all
      .filter((l) => streams[l.stream])
      .map((l) => ({ ...l, text: stripAnsi(l.text) }))
      .filter((l) => !q || l.text.toLowerCase().includes(q));
  }, [logs, query, streams]);

  useEffect(() => {
    const el = box.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setHeight(el.clientHeight));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  useLayoutEffect(() => {
    const el = box.current;
    if (el && follow) el.scrollTop = el.scrollHeight;
  }, [lines.length, follow, wrap]);

  const onScroll = () => {
    const el = box.current;
    if (!el) return;
    setScrollTop(el.scrollTop);
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < LINE_HEIGHT * 2;
    if (!atBottom && follow) setFollow(false);
    if (atBottom && !follow && active) setFollow(true);
  };

  const shown: LogLine[] = wrap ? lines.slice(-WRAP_LIMIT) : lines;
  const first = wrap ? 0 : Math.max(0, Math.floor(scrollTop / LINE_HEIGHT) - OVERSCAN);
  const last = wrap
    ? shown.length
    : Math.min(shown.length, Math.ceil((scrollTop + height) / LINE_HEIGHT) + OVERSCAN);
  const total = logs?.lines.length ?? 0;
  const copyText = () => send({ type: 'copy', text: lines.map((l) => l.text).join('\n') });

  return (
    <div className="logs">
      <div className="logs-toolbar">
        <div className="search-wrap">
          <Icon name="search" />
          <input
            type="search"
            placeholder="Filter log lines"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            aria-label="Filter log lines"
          />
        </div>
        {(['stdout', 'stderr', 'system'] as const).map((s) => (
          <label key={s} className={cx('check', `stream-${s}`)}>
            <input
              type="checkbox"
              checked={streams[s]}
              onChange={(e) => setStreams({ ...streams, [s]: e.target.checked })}
            />{' '}
            {s}
          </label>
        ))}
        <span className="spacer" />
        <span className="muted small">
          {query ? `${lines.length} of ${total} lines` : `${total} lines`}
          {logs?.ended ? '' : active ? ' · live' : ''}
        </span>
        <Button
          kind="ghost"
          icon={wrap ? 'word-wrap' : 'list-flat'}
          title={wrap ? `Wrapping (last ${WRAP_LIMIT} lines); click to stop` : 'Wrap long lines'}
          onClick={() => setWrap(!wrap)}
        />
        <Button kind="ghost" icon="copy" title="Copy visible lines" onClick={copyText} />
        <Button
          kind={follow ? 'secondary' : 'ghost'}
          icon="arrow-down"
          title={follow ? 'Following new output' : 'Follow new output'}
          onClick={() => setFollow(!follow)}
        />
      </div>
      <div
        className={cx('log-box', wrap && 'log-wrap')}
        ref={box}
        onScroll={onScroll}
        tabIndex={0}
        aria-label="Job log"
      >
        {!logs ? (
          <div className="log-placeholder">
            <Icon name="loading" spin /> Loading logs…
          </div>
        ) : shown.length === 0 ? (
          <div className="log-placeholder">
            {total === 0
              ? active
                ? 'Waiting for output…'
                : 'No output.'
              : 'No lines match the filter.'}
          </div>
        ) : wrap ? (
          shown.map((l, i) => (
            <div key={i} className={cx('log-line', `log-${l.stream}`)}>
              {renderLine(l.text, jobId, query)}
            </div>
          ))
        ) : (
          <div style={{ height: shown.length * LINE_HEIGHT, position: 'relative' }}>
            {shown.slice(first, last).map((l, i) => (
              <div
                key={first + i}
                className={cx('log-line', `log-${l.stream}`)}
                style={{
                  position: 'absolute',
                  top: (first + i) * LINE_HEIGHT,
                  height: LINE_HEIGHT,
                }}
              >
                {renderLine(l.text, jobId, query)}
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
