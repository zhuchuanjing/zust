local n = tonumber(arg[1])
local s = ""
local sep = ","
local chunk = "hello"
for i = 0, n - 1 do
    if i > 0 then s = s .. sep end
    s = s .. chunk
end
print(#s)
