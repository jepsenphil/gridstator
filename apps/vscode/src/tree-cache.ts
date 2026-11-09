import * as vscode from 'vscode';

export interface TreeRow<T> {
  element: T;
  item: vscode.TreeItem;
  parent?: string;
}

interface CachedRow<T> extends TreeRow<T> {
  signature: string;
}

/** Keeps native tree handles stable and reserves loading indicators for structural changes. */
export class TreeCache<T extends object> implements vscode.TreeDataProvider<T>, vscode.Disposable {
  private readonly emitter = new vscode.EventEmitter<T | T[] | undefined>();
  readonly onDidChangeTreeData = this.emitter.event;
  private rows = new Map<string, CachedRow<T>>();
  private children = new Map<string | undefined, string[]>();
  private readonly handles = new WeakMap<T, CachedRow<T>>();

  getChildren(element?: T): T[] {
    const id = element ? this.handles.get(element)?.item.id : undefined;
    if (element && id === undefined) return [];
    return (this.children.get(id) ?? []).map((child) => this.rows.get(child)!.element);
  }

  getTreeItem(element: T): vscode.TreeItem {
    return this.handles.get(element)!.item;
  }

  update(values: TreeRow<T>[]): void {
    const rows = new Map<string, CachedRow<T>>();
    const children = new Map<string | undefined, string[]>();
    const changed = new Set<string>();
    for (const value of values) {
      const id = value.item.id;
      if (!id) throw new Error('Cached tree rows require an explicit id');
      const item = value.item;
      // Command arguments carry the latest model, but invisible changes such as log sequence
      // numbers and heartbeat timestamps do not require repainting the row.
      const signature = JSON.stringify([
        item.label,
        item.description,
        item.iconPath,
        item.tooltip,
        item.contextValue,
        item.collapsibleState,
        item.command?.command,
        item.command?.title,
        item.resourceUri,
        item.accessibilityInformation,
      ]);
      const previous = this.rows.get(id);
      if (!previous || previous.signature !== signature) changed.add(id);
      const row = previous ?? { ...value, signature };
      row.item = item;
      row.parent = value.parent;
      row.signature = signature;
      rows.set(id, row);
      this.handles.set(row.element, row);
      const siblings = children.get(value.parent) ?? [];
      siblings.push(id);
      children.set(value.parent, siblings);
    }

    let rootChanged = false;
    for (const parent of new Set([...this.children.keys(), ...children.keys()])) {
      const before = this.children.get(parent) ?? [];
      const after = children.get(parent) ?? [];
      if (before.length === after.length && before.every((id, i) => id === after[i])) continue;
      if (parent === undefined) rootChanged = true;
      else if (rows.has(parent)) changed.add(parent);
    }
    this.rows = rows;
    this.children = children;
    if (rootChanged) {
      this.emitter.fire(undefined);
      return;
    }

    const targets: T[] = [];
    for (const id of changed) {
      const row = rows.get(id);
      if (!row) continue;
      let parent = row.parent;
      while (parent !== undefined && !changed.has(parent)) parent = rows.get(parent)?.parent;
      if (parent === undefined) targets.push(row.element);
    }
    if (targets.length === 1) this.emitter.fire(targets[0]);
    else if (targets.length) this.emitter.fire(targets);
  }

  dispose(): void {
    this.emitter.dispose();
    this.rows.clear();
    this.children.clear();
  }
}
