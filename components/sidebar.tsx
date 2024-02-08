import { BsGrid, BsGraphUp, BsHddStack, BsShieldCheck, BsBell, BsEnvelope, BsGear, BsBoxArrowRight } from "react-icons/bs";
import Image from 'next/image';
import logo from '../public/images/logo.png';
import { useEffect } from 'react';

function SideBar() {
  return (
    <div className=" top-0 left-0 h-screen w-1/10 flex flex-col bd relative ">
      <div className="bg-secondary shadow-lg p-5 h-screen">
      <a href="/" className="flex flex-row items-center justify-between gap-1 mb-10 mt-5 font-semibold">
        <Image className="align-middle w-5 md:w-10" src={logo} alt="logo" />
        <span className="text-quinary text-2xl hidden md:flex">MonitorFlow</span>
      </a>
      <SideBarTitle title="Main navigation" />
      <SideBarButton icon={<BsGrid size="20" />} text="Dashboard" link="/"/>
      <SideBarButton icon={<BsGraphUp size="20" />} text="Analytics" link="/analytics"/>
      <SideBarButton icon={<BsHddStack size="20" />} text="Servers" link="/servers"/>
      <SideBarTitle title="Notifications" />
      <SideBarButton icon={<BsShieldCheck size="20" />} text="Security" link="/security"/>
      <SideBarButton icon ={<BsBell size="20" />} text="Alerts" link="/alerts"/>
      <SideBarButton icon={<BsEnvelope size="20" />} text="Messages" link="/messages"/>
      <SideBarTitle title="Settings" />
      <SideBarButton icon={<BsGear size="20" />} text="Preferences" link="/preferences"/>
      <SideBarButton icon={<BsBoxArrowRight size="20" />} text={"Sign out"} link="/sign-out"/>
      </div>
      <div className="w-10">
        
      </div>
    </div>
  );
}

const SideBarButton = ({ icon, text, link }: { icon: JSX.Element, text: string, link:string }) => {
    return (
        <a href={link} className="flex pt-5 pb-5 text-quinary hover:text-tertiary cursor-pointer font-medium ">
            {icon}
            <span className="text-sm pl-2 hidden md:flex">{text}</span>
        </a>
    );
}

const SideBarTitle = ({ title }: { title: string }) => {
    return (
        <div className=" flex-row uppercase pt-3 pb-3 hidden md:flex">
            <span className="text-senary text-base font-medium">{title}</span>
        </div>
    );
}

export default SideBar;
