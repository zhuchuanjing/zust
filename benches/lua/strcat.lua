local n = tonumber(arg[1])
local s = ""
for _ = 1, n do
    s = s .. "x"
end
print(#s)
