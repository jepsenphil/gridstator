export function add(a, b) {
  return a + b;
}

export function fib(n) {
  return n < 2 ? n : fib(n - 1) + fib(n - 2);
}
