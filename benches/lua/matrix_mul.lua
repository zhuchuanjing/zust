local n = tonumber(arg[1])
local sz = 40
local a, b, c = {}, {}, {}
for i = 0, sz - 1 do
    for j = 0, sz - 1 do
        a[i * sz + j + 1] = (i * j) * 0.01
        b[i * sz + j + 1] = (i + j) * 0.005
    end
end
for i = 0, sz - 1 do
    for j = 0, sz - 1 do
        c[i * sz + j + 1] = 0.0
    end
end
for _ = 1, n do
    for i = 0, sz - 1 do
        for k = 0, sz - 1 do
            local aik = a[i * sz + k + 1]
            for j = 0, sz - 1 do
                c[i * sz + j + 1] = c[i * sz + j + 1] + aik * b[k * sz + j + 1]
            end
        end
    end
end
print(math.floor(c[1]))
