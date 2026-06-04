import sys

def ack(m, n):
    if m == 0:
        return n + 1
    if n == 0:
        return ack(m - 1, 1)
    return ack(m - 1, ack(m, n - 1))

n = int(sys.argv[1]) if len(sys.argv) > 1 else 6
print(ack(3, n))
