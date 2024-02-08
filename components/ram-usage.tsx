
function RamUsage() {
  return ( 
    <div className="bg-gray-100 p-4 rounded-lg shadow-md">
      <h2 className="text-xl font-semibold">RAM Usage</h2>
      <div className="flex justify-between items-center mt-4">
        <div className="w-32 h-32 bg-primary rounded-lg flex justify-center items-center">
          <span className="text-2xl font-bold text-white">60%</span>
        </div>
        <div className="flex flex-col">
          <span className="text-sm font-medium">Total: 16GB</span>
          <span className="text-sm font-medium">Used: 10GB</span>
          <span className="text-sm font-medium">Free: 6GB</span>
        </div>
      </div>
    </div>
  );
}

export default RamUsage;



