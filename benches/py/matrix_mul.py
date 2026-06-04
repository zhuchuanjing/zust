import sys
n = int(sys.argv[1])
sz = 40
sz2 = sz * sz
a = [0.0] * sz2
b = [0.0] * sz2
c = [0.0] * sz2
for i in range(sz):
    for j in range(sz):
        a[i * sz + j] = (i * j) * 0.01
        b[i * sz + j] = (i + j) * 0.005
for _ in range(n):
    for i in range(sz):
        for k in range(sz):
            aik = a[i * sz + k]
            base = i * sz
            bk_base = k * sz
            for j in range(sz):
                c[base + j] += aik * b[bk_base + j]
print(int(c[0]))
