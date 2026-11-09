import type { WebSocket } from 'ws';
import type { Job, LogEntry, Node, ServerEventMessage } from '@gridstator/protocol';

interface Subscriber {
  socket: WebSocket;
  topics: Set<'jobs' | 'nodes'>;
  logs: Set<string>;
}

/** Fans out job, node and log events to connected clients (the /v1/events stream). */
export class EventHub {
  private readonly subscribers = new Set<Subscriber>();

  add(socket: WebSocket): Subscriber {
    const sub: Subscriber = { socket, topics: new Set(), logs: new Set() };
    this.subscribers.add(sub);
    socket.on('close', () => this.subscribers.delete(sub));
    return sub;
  }

  send(sub: Subscriber, msg: ServerEventMessage): void {
    if (sub.socket.readyState === 1) sub.socket.send(JSON.stringify(msg));
  }

  jobUpdated(job: Job): void {
    const msg = JSON.stringify({ type: 'job.updated', job } satisfies ServerEventMessage);
    for (const s of this.subscribers)
      if (s.topics.has('jobs') && s.socket.readyState === 1) s.socket.send(msg);
  }

  nodeUpdated(node: Node): void {
    const msg = JSON.stringify({ type: 'node.updated', node } satisfies ServerEventMessage);
    for (const s of this.subscribers)
      if (s.topics.has('nodes') && s.socket.readyState === 1) s.socket.send(msg);
  }

  nodeRemoved(nodeId: string): void {
    const msg = JSON.stringify({ type: 'node.removed', nodeId } satisfies ServerEventMessage);
    for (const s of this.subscribers)
      if (s.topics.has('nodes') && s.socket.readyState === 1) s.socket.send(msg);
  }

  logs(jobId: string, entries: LogEntry[]): void {
    if (entries.length === 0) return;
    const msg = JSON.stringify({ type: 'logs', jobId, entries } satisfies ServerEventMessage);
    for (const s of this.subscribers)
      if (s.logs.has(jobId) && s.socket.readyState === 1) s.socket.send(msg);
  }

  logsEnded(job: Job): void {
    const msg = JSON.stringify({
      type: 'logs.end',
      jobId: job.id,
      state: job.state,
    } satisfies ServerEventMessage);
    for (const s of this.subscribers) {
      if (s.logs.has(job.id)) {
        if (s.socket.readyState === 1) s.socket.send(msg);
        s.logs.delete(job.id);
      }
    }
  }

  closeAll(): void {
    for (const s of this.subscribers) s.socket.close(1001, 'controller shutting down');
  }
}

export type { Subscriber };
