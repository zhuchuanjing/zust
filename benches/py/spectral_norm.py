import sys
import math

def eval_A(i, j):
    return 1.0 / ((i + j) * (i + j + 1) / 2 + i + 1)

def multiply_Av(n, v):
    result = [0.0] * n
    for i in range(n):
        s = 0.0
        for j in range(n):
            s += eval_A(i, j) * v[j]
        result[i] = s
    return result

def multiply_Atv(n, v):
    result = [0.0] * n
    for i in range(n):
        s = 0.0
        for j in range(n):
            s += eval_A(j, i) * v[j]
        result[i] = s
    return result

n = int(sys.argv[1])
u = [1.0] * n
v = [0.0] * n
for _ in range(10):
    v = multiply_Av(n, u)
    u = multiply_Atv(n, v)
vBv = 0.0
vv = 0.0
for i in range(n):
    vBv += u[i] * v[i]
    vv += v[i] * v[i]
result = math.sqrt(vBv / vv)
print(int(result * 1000000))
